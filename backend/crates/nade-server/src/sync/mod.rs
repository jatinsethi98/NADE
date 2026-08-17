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
//! 3. **Batch `format=raw`, 45 per batch, at most one batch per second.**
//! 4. **A parse failure writes a metadata-only row plus an audit entry and the
//!    sync carries on.** One unreadable message must never cost the mailbox.
//! 5. **A message that 404s between list and get is normal** - the user deleted
//!    it. Skip it; do not retry it; do not fail.
//!
//! And the rule with a number attached to it: **ingest never calls an LLM.** Not
//! once, not for one field. The prior art coupled them and lost 13% of its data
//! when the model was slow or unavailable. [`tests::ingest_never_calls_an_llm`]
//! enforces it by grep.

pub mod store;

use std::{sync::Arc, time::Duration};

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

/// Knobs, all of which PLAN.md fixes in production.
#[derive(Debug, Clone)]
pub struct SyncOptions {
    /// `newer_than:30d`.
    pub query: String,
    /// `MAX_SYNC_MESSAGES`.
    pub max_messages: usize,
    /// 45.
    pub batch_size: usize,
    /// At most one batch per second. Tests shrink it; nothing else may.
    pub batch_interval: Duration,
}

impl SyncOptions {
    #[must_use]
    pub fn from_config(config: &crate::config::Config) -> Self {
        Self {
            query: format!("newer_than:{}d", config.gmail.sync_window_days),
            max_messages: config.gmail.max_sync_messages,
            batch_size: config.gmail.batch_size,
            batch_interval: Duration::from_secs(1),
        }
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
    /// Could not be fetched at all; left for the next sync.
    pub fetch_failures: usize,
    pub threads: usize,
    pub history_id: Option<i64>,
    /// True when the account is `needs_reauth` and the sync did nothing.
    pub paused: bool,
}

/// Run one full sync.
///
/// # Errors
/// Returns an error only for a failure that makes the whole sync meaningless -
/// the profile read, the listing, or a database write. Per-message failures are
/// counted and audited, never propagated.
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

    // STEP 3 - batches of 45, paced.
    let mut touched_threads: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut pacer = Pacer::new(options.batch_interval);

    for chunk in listed.chunks(options.batch_size.max(1)) {
        pacer.wait().await;
        report.batches += 1;

        let ids: Vec<String> = chunk.iter().map(|entry| entry.id.clone()).collect();
        let outcomes = match client.batch_get_raw(&ids).await {
            Ok(outcomes) => outcomes,
            Err(error) => {
                // A whole batch failing is survivable: audit it and take the
                // next one. The listing is cheap to redo on the next sync.
                tracing::warn!(%error, count = ids.len(), "a batch failed; continuing");
                report.fetch_failures += ids.len();
                store::audit(
                    pool,
                    account_id,
                    "gmail_batch_failed",
                    json!({ "count": ids.len(), "error": error.to_string() }),
                )
                .await?;
                continue;
            }
        };

        for outcome in outcomes {
            match outcome {
                BatchOutcome::Fetched {
                    gmail_id,
                    message,
                    raw,
                } => {
                    let row = match parse::parse(&raw, &gmail_id) {
                        Ok(parsed) => store::IngestRow::parsed(&message, parsed),
                        Err(error) => {
                            // STEP 4 - a metadata-only row, an audit entry, and
                            // on we go.
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

                    touched_threads.insert(row.thread_id.clone());
                    report.ingested += 1;
                }
                // STEP 5 - the user deleted it. Not an error, not a retry.
                BatchOutcome::Gone { gmail_id } => {
                    report.vanished += 1;
                    tracing::debug!(%gmail_id, "message vanished between list and get; skipping");
                }
                BatchOutcome::Failed {
                    gmail_id,
                    status,
                    detail,
                } => {
                    report.fetch_failures += 1;
                    tracing::warn!(%gmail_id, status, %detail, "could not fetch a message; continuing");
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

    // Rollups last, once per touched thread rather than once per message.
    for thread_id in &touched_threads {
        let mut tx = pool.begin().await?;
        store::refresh_thread(&mut tx, account_id, thread_id).await?;
        tx.commit().await?;
    }
    report.threads = touched_threads.len();

    // The cursor read in step 1, committed now that the window is on disk.
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
            "threads": report.threads,
            "batches": report.batches,
            "history_id": report.history_id,
        }),
    )
    .await?;

    tracing::info!(?report, "gmail sync finished");
    Ok(report)
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
