//! The initial 30-day sync, as a job queue `kind`.
//!
//! PLAN.md §Gmail sync 2, in order and for a reason each:
//!
//! 1. **`getProfile`'s `historyId` first, then list.** Reading it after the list
//!    would leave a window in which a message arrives, misses the list, and is
//!    also older than the recorded history cursor - a gap that no later
//!    incremental sync ever closes. Reading it first can only cause an overlap,
//!    and an overlap is an upsert.
//! 2. **`newer_than:30d`, capped at `MAX_SYNC_MESSAGES`.** Dev caps are law.
//! 3. **Batch `format=raw`, at most one batch per second.** PLAN.md said 45 per
//!    batch, sized against the 250 units/second quota; the live sync showed
//!    that is the wrong ceiling, because Google runs a batch's sub-requests
//!    concurrently. See [`crate::gmail::client::MAX_BATCH`].
//! 4. **A parse failure writes a metadata-only row plus an audit entry and the
//!    sync carries on.** One unreadable message must never cost the mailbox.
//! 5. **A message that 404s between list and get is normal** - the user deleted
//!    it. Skip it; do not retry it; do not fail.
//! 6. **The cursor never moves over a message that is not resolved.** Added
//!    after the first live sync, which is the only rule here that was learned
//!    rather than designed. Gmail answered 91 sub-requests with
//!    `429 "Too many concurrent requests for user."`; each was logged, audited
//!    and *skipped*, and `record_history_id` then advanced the cursor past
//!    them - so a temporary throttle would have permanently deleted 77
//!    messages from NADE's view of the mailbox. A transient failure is now
//!    retried, and a sync that still cannot account for every listed message
//!    **fails the job** rather than committing progress it did not make.
//!
//! And the rule with a number attached to it: **ingest never calls an LLM.** Not
//! once, not for one field. The prior art coupled them and lost 13% of its data
//! when the model was slow or unavailable. [`tests::ingest_never_calls_an_llm`]
//! enforces it by grep.

pub mod store;

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    gmail::client::{BatchOutcome, GmailClient},
    jobs::{Handler, Job, JobContext},
    mail::parse,
    state::AppState,
};

/// The registered job kind.
pub const KIND: &str = "gmail_sync";

/// How many times a transiently-failed message is re-asked for inside one sync
/// before the sync gives up and lets the queue's own backoff take over.
///
/// Four rounds at 2 s doubling is ~30 s of in-sync retrying: long enough to ride
/// out a Gmail throttle, short enough to stay well inside the job lease.
pub const MAX_RETRY_ROUNDS: u32 = 4;

/// Knobs, all of which PLAN.md fixes in production.
#[derive(Debug, Clone)]
pub struct SyncOptions {
    /// `newer_than:30d`.
    ///
    /// **A rolling 30 x 24h window measured from the instant the request lands,
    /// not 30 calendar days.** Probed against the live API: `newer_than:1d`
    /// returns exactly `after:<now - 24h>`, and disagrees with both a
    /// midnight-UTC and a midnight-Pacific reading. Nothing here may assume a
    /// day boundary, and nothing may assume two syncs an hour apart see the same
    /// window - which is the other half of why the history cursor is read before
    /// the listing rather than after it.
    pub query: String,
    /// `MAX_SYNC_MESSAGES`.
    pub max_messages: usize,
    /// Messages per batch, capped at [`crate::gmail::client::MAX_BATCH`].
    pub batch_size: usize,
    /// At most one batch per second. Tests shrink it; nothing else may.
    pub batch_interval: Duration,
    /// Re-ask rounds for transiently-failed messages.
    pub max_retry_rounds: u32,
    /// Base delay before the first re-ask; doubled each round. Tests shrink it.
    pub retry_backoff: Duration,
}

impl SyncOptions {
    /// Build the production options.
    ///
    /// The query is a **fixed template over one validated integer** and never
    /// concatenates anything a user or a message supplied. That is deliberate,
    /// because Gmail gives no way to detect the mistake: probed against the live
    /// API, an unknown operator, an empty `label:` and a malformed query all
    /// return `200` with zero messages, identical to a genuinely empty window.
    /// A sync that built its query dynamically could list nothing, ingest
    /// nothing, and report success forever.
    ///
    /// It also carries no scope operator. `includeSpamTrash=false` - the default
    /// this client relies on - gates neither `q` nor `labelIds`: `in:anywhere`,
    /// `in:trash` and `in:spam` all widen the scope straight past it. Keeping
    /// Trash and Spam out is therefore a property of *not putting an `in:` in
    /// the query*, not a property of the parameter.
    /// [`tests::the_sync_query_is_a_fixed_template_with_no_scope_operator`]
    /// holds both halves.
    #[must_use]
    pub fn from_config(config: &crate::config::Config) -> Self {
        Self {
            query: format!("newer_than:{}d", config.gmail.sync_window_days),
            max_messages: config.gmail.max_sync_messages,
            batch_size: config.gmail.batch_size,
            batch_interval: Duration::from_secs(1),
            max_retry_rounds: MAX_RETRY_ROUNDS,
            retry_backoff: Duration::from_secs(2),
        }
    }

    /// The delay before re-asking for the messages a throttle cost us.
    fn retry_delay(&self, round: u32) -> Duration {
        self.retry_backoff
            .saturating_mul(1u32 << round.min(5))
            .min(crate::gmail::quota::MAX_BACKOFF)
    }
}

/// What one sync did. Written to `audit_log` and returned for the tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub labels: usize,
    pub listed: usize,
    pub batches: usize,
    pub ingested: usize,
    /// Deleted between `messages.list` and `messages.get`. Normal.
    pub vanished: usize,
    /// Stored as a metadata-only row because the MIME would not parse.
    pub parse_failures: usize,
    /// Permanently unfetchable - a `400`, a `403` that is not a throttle. The
    /// cursor is allowed to move past these, because asking again cannot help.
    pub fetch_failures: usize,
    /// Messages a transient failure cost us on one pass and a later round
    /// recovered. Non-zero means the throttle happened and was absorbed.
    pub retried: usize,
    /// How many re-ask rounds it took.
    pub retry_rounds: u32,
    /// Messages still unfetched when the sync stopped. **Non-zero is a failed
    /// sync**: the cursor is left where it was and the job returns `Err`.
    pub unresolved: usize,
    pub threads: usize,
    pub history_id: Option<i64>,
    /// True when the account is `needs_reauth` and the sync did nothing.
    pub paused: bool,
    /// True when another worker already holds this account's sync lock.
    pub skipped_concurrent: bool,
}

/// The sync could not account for every message it listed.
///
/// A distinct type rather than a string, because the handler and the tests both
/// have to be able to tell "Gmail threw us out" apart from "the database is
/// down": the first must leave the cursor alone and retry, and a caller that
/// cannot distinguish them will eventually paper over one with the other.
#[derive(Debug, thiserror::Error)]
#[error(
    "gmail sync incomplete: {unresolved} of {listed} listed messages are still unfetched \
     after {rounds} retry rounds; the history cursor was NOT advanced, so the queue's \
     retry will fetch them"
)]
pub struct SyncIncomplete {
    pub listed: usize,
    pub unresolved: usize,
    pub rounds: u32,
}

/// Run one full sync.
///
/// # Errors
/// Returns an error for a failure that makes the whole sync meaningless - the
/// profile read, the listing, or a database write - **and** for a sync that
/// finished with messages still unfetched ([`SyncIncomplete`]). The second is
/// the important one: it is what stops a transient Gmail throttle from becoming
/// permanent data loss, because the cursor is only written on the path that
/// returns `Ok`.
pub async fn run_sync(
    pool: &PgPool,
    client: &GmailClient,
    account_id: Uuid,
    options: &SyncOptions,
) -> Result<SyncReport> {
    let mut report = SyncReport::default();

    // Paused accounts do nothing at all, quietly and successfully: failing the
    // job would just burn the retry budget until the human re-consents.
    let status: Option<String> = sqlx::query_scalar("select status from accounts where id = $1")
        .bind(account_id)
        .fetch_optional(pool)
        .await?;
    match status.as_deref() {
        None => {
            tracing::warn!(%account_id, "sync asked for an account that does not exist");
            report.paused = true;
            return Ok(report);
        }
        Some("needs_reauth") => {
            tracing::info!(%account_id, "gmail sync is paused: the account needs re-consent");
            report.paused = true;
            return Ok(report);
        }
        Some(_) => {}
    }

    // One sync per account at a time. Gmail rations *concurrent* requests per
    // user, so two syncs of one mailbox do not run twice as fast - they throttle
    // each other and pay twice for the same mail. Nothing in `jobs.rs` dedupes
    // by payload, so this is where it has to be said.
    let Some(lock) = AccountLock::take(pool, account_id).await? else {
        tracing::info!(%account_id, "another worker is already syncing this account; nothing to do");
        report.skipped_concurrent = true;
        return Ok(report);
    };

    // The lock is released on every path out of `sync_window`, success or not.
    let outcome = sync_window(pool, client, account_id, options, &mut report).await;
    lock.release().await;
    outcome?;
    Ok(report)
}

/// Steps 1-6, with the account lock already held.
async fn sync_window(
    pool: &PgPool,
    client: &GmailClient,
    account_id: Uuid,
    options: &SyncOptions,
    report: &mut SyncReport,
) -> Result<()> {
    // STEP 1 - the history cursor, BEFORE the listing. Overlap, never a gap.
    let profile = client
        .get_profile()
        .await
        .context("reading the Gmail profile")?;
    let history_id = profile.history_id.as_deref().and_then(|id| id.parse().ok());
    report.history_id = history_id;

    let labels = client.list_labels().await.context("listing Gmail labels")?;
    report.labels = store::upsert_labels(pool, account_id, &labels).await?;

    // STEP 2 - the dev caps.
    let listed = client
        .list_message_ids(&options.query, options.max_messages)
        .await
        .context("listing Gmail messages")?;
    report.listed = listed.len();

    // STEP 3 - paced batches, then as many re-ask rounds as it takes.
    let mut touched_threads: BTreeSet<String> = BTreeSet::new();
    let mut pacer = Pacer::new(options.batch_interval);
    let mut pending: Vec<String> = listed.iter().map(|entry| entry.id.clone()).collect();
    let mut round = 0u32;

    loop {
        if pending.is_empty() {
            break;
        }
        if round > 0 {
            // A real backoff, not a tight loop: the whole point of a retry here
            // is to arrive after the throttle has lifted.
            let delay = options.retry_delay(round - 1);
            tracing::warn!(
                round,
                pending = pending.len(),
                wait_ms = delay.as_millis(),
                "gmail could not serve some messages yet; backing off and re-asking"
            );
            store::audit(
                pool,
                account_id,
                "gmail_sync_throttled",
                json!({ "round": round, "pending": pending.len(), "wait_ms": delay.as_millis() }),
            )
            .await?;
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            report.retry_rounds = round;
        }

        let mut still_pending: Vec<String> = Vec::new();

        for chunk in pending.chunks(options.batch_size.max(1)) {
            pacer.wait().await;
            report.batches += 1;

            let outcomes = match client.batch_get_raw(chunk).await {
                Ok(outcomes) => outcomes,
                Err(error) => {
                    // A whole batch failing leaves every message in it
                    // unresolved. Auditing it and moving on is fine; *forgetting
                    // it* is not, so the ids stay pending.
                    tracing::warn!(%error, count = chunk.len(), "a batch failed; re-queueing its messages");
                    still_pending.extend(chunk.iter().cloned());
                    store::audit(
                        pool,
                        account_id,
                        "gmail_batch_failed",
                        json!({ "count": chunk.len(), "error": error.to_string() }),
                    )
                    .await?;
                    continue;
                }
            };

            // Rollups per batch rather than once at the end. The first live sync
            // ingested 176 messages and then died before its single trailing
            // rollup loop, leaving `threads` empty and the mail list blank
            // despite a full `messages` table.
            let mut batch_threads: BTreeSet<String> = BTreeSet::new();

            for outcome in outcomes {
                match outcome {
                    BatchOutcome::Fetched {
                        gmail_id,
                        message,
                        raw,
                    } => {
                        if round > 0 {
                            report.retried += 1;
                        }
                        let row = match parse::parse(&raw, &gmail_id) {
                            Ok(parsed) => store::IngestRow::parsed(&message, parsed),
                            Err(error) => {
                                // STEP 4 - a metadata-only row, an audit entry,
                                // and on we go.
                                report.parse_failures += 1;
                                tracing::warn!(%gmail_id, %error, "unparseable message; storing metadata only");
                                store::audit(
                                    pool,
                                    account_id,
                                    "message_parse_failed",
                                    json!({ "gmail_id": gmail_id, "error": error.to_string() }),
                                )
                                .await?;
                                store::IngestRow::metadata_only(&message)
                            }
                        };

                        let mut tx = pool.begin().await?;
                        store::upsert_message(&mut tx, account_id, &row).await?;
                        tx.commit().await?;

                        batch_threads.insert(row.thread_id.clone());
                        report.ingested += 1;
                    }
                    // STEP 5 - the user deleted it. Not an error, not a retry.
                    BatchOutcome::Gone { gmail_id } => {
                        report.vanished += 1;
                        tracing::debug!(%gmail_id, "message vanished between list and get; skipping");
                    }
                    // STEP 6 - the distinction the first live sync did not make.
                    BatchOutcome::Failed {
                        gmail_id,
                        status,
                        detail,
                        transient,
                    } => {
                        if transient {
                            tracing::debug!(%gmail_id, status, "transient fetch failure; will re-ask");
                            still_pending.push(gmail_id);
                        } else {
                            report.fetch_failures += 1;
                            tracing::warn!(%gmail_id, status, %detail, "permanently could not fetch a message");
                            store::audit(
                                pool,
                                account_id,
                                "message_fetch_failed",
                                json!({ "gmail_id": gmail_id, "status": status, "detail": detail }),
                            )
                            .await?;
                        }
                    }
                }
            }

            for thread_id in &batch_threads {
                let mut tx = pool.begin().await?;
                store::refresh_thread(&mut tx, account_id, thread_id).await?;
                tx.commit().await?;
            }
            touched_threads.extend(batch_threads);
        }

        pending = still_pending;
        if pending.is_empty() || round >= options.max_retry_rounds {
            break;
        }
        round += 1;
    }

    report.threads = touched_threads.len();
    report.unresolved = pending.len();

    // The one branch this whole module exists for.
    if !pending.is_empty() {
        let sample: Vec<&String> = pending.iter().take(20).collect();
        store::audit(
            pool,
            account_id,
            "gmail_sync_incomplete",
            json!({
                "listed": report.listed,
                "ingested": report.ingested,
                "unresolved": report.unresolved,
                "retry_rounds": report.retry_rounds,
                "threads": report.threads,
                "batches": report.batches,
                "history_id_withheld": report.history_id,
                "unresolved_sample": sample,
            }),
        )
        .await?;
        tracing::error!(
            ?report,
            "gmail sync gave up with messages unfetched; leaving the cursor where it was"
        );
        return Err(SyncIncomplete {
            listed: report.listed,
            unresolved: report.unresolved,
            rounds: report.retry_rounds,
        }
        .into());
    }

    // The cursor read in step 1, committed now that the window is on disk -
    // and only now, because everything listed is either stored, deleted, or
    // permanently unfetchable.
    store::record_history_id(pool, account_id, history_id).await?;
    store::audit(
        pool,
        account_id,
        "gmail_sync_completed",
        json!({
            "listed": report.listed,
            "ingested": report.ingested,
            "vanished": report.vanished,
            "parse_failures": report.parse_failures,
            "fetch_failures": report.fetch_failures,
            "retried": report.retried,
            "retry_rounds": report.retry_rounds,
            "threads": report.threads,
            "batches": report.batches,
            "history_id": report.history_id,
        }),
    )
    .await?;

    tracing::info!(?report, "gmail sync finished");
    Ok(())
}

/// A Postgres advisory lock held for one account's sync.
///
/// Session-scoped, so the connection is held for the sync's lifetime and the
/// lock disappears by itself if the process dies - which is exactly the property
/// a `syncing` column would not have, and the reason a table flag was not used.
struct AccountLock {
    connection: sqlx::pool::PoolConnection<sqlx::Postgres>,
    key: i32,
}

/// Namespace half of the advisory-lock key: `"NADE"` as big-endian ASCII, so a
/// future lock in another subsystem cannot collide with this one by accident.
pub(crate) const SYNC_LOCK_NAMESPACE: i32 = i32::from_be_bytes(*b"NADE");

/// The account half of the key. The first four bytes of a v4 UUID are random,
/// which is all a lock key needs; a collision between two accounts would only
/// serialise two syncs that were already sharing one Gmail quota.
pub(crate) fn account_lock_key(account_id: Uuid) -> i32 {
    let bytes = account_id.as_bytes();
    i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

impl AccountLock {
    async fn take(pool: &PgPool, account_id: Uuid) -> Result<Option<Self>> {
        let key = account_lock_key(account_id);
        let mut connection = pool.acquire().await?;
        let taken: bool = sqlx::query_scalar("select pg_try_advisory_lock($1, $2)")
            .bind(SYNC_LOCK_NAMESPACE)
            .bind(key)
            .fetch_one(&mut *connection)
            .await?;
        Ok(taken.then_some(Self { connection, key }))
    }

    /// Give the lock back before the connection returns to the pool. A pooled
    /// connection is *recycled*, not closed, so its session - and therefore its
    /// locks - would otherwise outlive the sync.
    async fn release(mut self) {
        if let Err(error) = sqlx::query("select pg_advisory_unlock($1, $2)")
            .bind(SYNC_LOCK_NAMESPACE)
            .bind(self.key)
            .execute(&mut *self.connection)
            .await
        {
            // Not fatal: the lock dies with the session, and the session dies
            // when the pool retires the connection or the process exits.
            tracing::warn!(%error, "could not release the account sync lock");
        }
    }
}

/// At most one batch per `interval`, measured monotonically.
///
/// EDGE (clock skew): `tokio::time::Instant` is monotonic, so a wall-clock jump
/// cannot make the pacer either stall for hours or fire in a burst.
struct Pacer {
    interval: Duration,
    next: Option<tokio::time::Instant>,
}

impl Pacer {
    fn new(interval: Duration) -> Self {
        Self {
            interval,
            next: None,
        }
    }

    async fn wait(&mut self) {
        if let Some(at) = self.next {
            tokio::time::sleep_until(at).await;
        }
        self.next = Some(tokio::time::Instant::now() + self.interval);
    }
}

// ------------------------------------------------------------- the job --

/// The `gmail_sync` handler. Holds its own dependencies, so `jobs.rs` stays
/// generic infrastructure that knows nothing about Gmail.
pub struct SyncHandler {
    state: AppState,
}

impl SyncHandler {
    /// Ready to hand to [`crate::jobs::Registry::register`].
    #[must_use]
    pub fn shared(state: AppState) -> Arc<dyn Handler> {
        Arc::new(Self { state })
    }
}

impl std::fmt::Debug for SyncHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SyncHandler")
    }
}

#[async_trait]
impl Handler for SyncHandler {
    async fn handle(&self, job: Job, _ctx: JobContext) -> Result<()> {
        // The payload may name an account; with none, we sync the single one
        // this server serves.
        let account_id = match job.payload.get("account_id").and_then(|v| v.as_str()) {
            Some(raw) => Uuid::parse_str(raw).context("the account_id in the job payload")?,
            None => match self.state.account().await? {
                Some(account) => account.id,
                None => {
                    tracing::info!("no Gmail account is connected yet; nothing to sync");
                    return Ok(());
                }
            },
        };

        let client = self.state.gmail.client_for(account_id);
        let options = SyncOptions::from_config(&self.state.config);
        run_sync(&self.state.pool, &client, account_id, &options).await?;
        Ok(())
    }
}

/// Enqueue a sync for the connected account.
///
/// # Errors
/// Returns an error if the enqueue fails.
pub async fn enqueue(queue: &crate::jobs::Queue, account_id: Uuid) -> sqlx::Result<i64> {
    queue
        .enqueue(KIND, &json!({ "account_id": account_id }), None)
        .await
}

#[cfg(test)]
mod tests;
