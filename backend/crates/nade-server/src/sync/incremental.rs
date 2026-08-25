//! The incremental history walk.
//!
//! P2 recorded a history cursor and never read it. This is the consumer: a
//! webhook (or the polling fallback) enqueues a job, and the job walks
//! `users.history.list` from the stored cursor, applying all four history types
//! until Gmail runs out of pages.
//!
//! Three rules govern the walk, and each of them is a silent bug when broken.
//!
//! 1. **`startHistoryId` is constant for the whole walk; only `pageToken`
//!    advances.** Gmail fingerprints a page token against the start id it was
//!    issued for and rejects a mismatched pair.
//! 2. **The stored cursor is `max(record.id)` over the records actually
//!    applied, never the page's top-level `historyId`.** See
//!    [`HistoryList::max_record_id`] for what happens otherwise; the short
//!    version is a `200` with no `history` key, indistinguishable from "nothing
//!    changed", with messages silently never ingested.
//! 3. **The network stays outside the transaction.** "Per-page transaction"
//!    means the page's *writes* are atomic. The pool is ten connections and a
//!    batch is paced; holding one through the fetch would starve every request
//!    handler in the process.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use std::sync::Arc;

use async_trait::async_trait;

use super::{store, AccountLock, Pacer, SyncOptions};
use crate::{
    gmail::{
        client::{BatchOutcome, GmailClient},
        types::HistoryList,
    },
    mail::parse,
};
use chrono::Utc;

use crate::{
    jobs::{Handler, Job, JobContext, Queue},
    state::AppState,
};

/// The job kind that walks history.
pub const KIND: &str = "gmail_incremental";

/// How many times a single job will re-walk before handing over.
///
/// Each pass costs at least one `history.list`, and a steady stream of
/// notifications would otherwise keep one job walking indefinitely while its
/// lease quietly expires underneath it. Handing back after a few passes lets
/// the queue re-claim normally; nothing is lost, because the flag that caused
/// the last pass is still set and the next job takes it.
const MAX_DRAIN_PASSES: usize = 8;

/// What one page of history asks us to do.
///
/// Produced by [`plan_page`], which is pure, so the folding rules below are
/// testable without a database or a server.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PagePlan {
    /// `messagesAdded` ids, minus anything a later record on the same page
    /// deleted.
    pub to_fetch: Vec<String>,
    /// The net label delta per message: `(added, removed)`.
    pub label_deltas: BTreeMap<String, (Vec<String>, Vec<String>)>,
    /// `messagesDeleted` ids.
    pub deleted: BTreeSet<String>,
    /// `max(record.id)` - the only legal cursor for this page.
    pub cursor: Option<i64>,
    /// How many history records the page carried.
    pub records: usize,
    /// Entries whose message id was missing or empty.
    ///
    /// Gmail has never sent one; a schema change or a corrupt response would.
    /// They are counted and audited rather than blocking the page, because the
    /// id is the *only* handle on the message: refusing to advance would
    /// re-read the same malformed record forever, wedging the account without
    /// recovering anything. The 30-day full sync and the reconciliation sweep
    /// are the backstops that can still find the message.
    pub malformed: usize,
}

/// Reduce one page to its net effect.
///
/// Records are folded in ascending id order with last-writer-wins per message,
/// so a page that adds `UNREAD` and then removes it produces one update rather
/// than two, and a message added and then deleted within the same page is never
/// fetched at all.
#[must_use]
pub fn plan_page(page: &HistoryList) -> PagePlan {
    let mut plan = PagePlan {
        cursor: page.max_record_id(),
        records: page.history.len(),
        ..PagePlan::default()
    };

    // Gmail does not promise an order, and the fold is only correct in one.
    let mut records: Vec<&crate::gmail::types::HistoryRecord> = page.history.iter().collect();
    records.sort_by_key(|record| record.id.as_deref().and_then(|id| id.parse::<i64>().ok()));

    let mut added_ids: Vec<String> = Vec::new();
    for record in records {
        for entry in &record.messages_added {
            let id = &entry.message.id;
            if id.is_empty() {
                plan.malformed += 1;
                continue;
            }
            plan.deleted.remove(id);
            if !added_ids.contains(id) {
                added_ids.push(id.clone());
            }
        }
        for entry in &record.messages_deleted {
            let id = &entry.message.id;
            if id.is_empty() {
                plan.malformed += 1;
                continue;
            }
            added_ids.retain(|candidate| candidate != id);
            plan.label_deltas.remove(id);
            plan.deleted.insert(id.clone());
        }
        for (entry, adding) in record
            .labels_added
            .iter()
            .map(|entry| (entry, true))
            .chain(record.labels_removed.iter().map(|entry| (entry, false)))
        {
            let id = &entry.message.id;
            if id.is_empty() {
                plan.malformed += 1;
                continue;
            }
            if plan.deleted.contains(id) {
                continue;
            }
            let delta = plan.label_deltas.entry(id.clone()).or_default();
            for label in &entry.label_ids {
                // A label cannot be both added and removed by the end of the
                // page; the later record wins, which is what these two lines
                // are for.
                let (this, other) = if adding {
                    (&mut delta.0, &mut delta.1)
                } else {
                    (&mut delta.1, &mut delta.0)
                };
                other.retain(|existing| existing != label);
                if !this.contains(label) {
                    this.push(label.clone());
                }
            }
        }
    }

    plan.to_fetch = added_ids;
    plan
}

/// What a walk did.
#[derive(Debug, Default, serde::Serialize)]
pub struct IncrementalReport {
    pub pages: usize,
    pub records: usize,
    /// The gmail ids `messagesAdded` produced and we ingested.
    ///
    /// **This was P5's seam, and it is closed**: the page transaction enqueues
    /// one `triage_message` per entry here, so the trigger commits with the
    /// mail that fires it. The list is still returned, and still asserted, for
    /// the reason it was returned in the first place - a no-op would make
    /// "the trigger fired and did nothing" indistinguishable from "the trigger
    /// fired".
    ///
    /// Note which list this is: what actually **parsed and was upserted**, not
    /// what the page asked Gmail for. `to_fetch` includes the ids that came
    /// back unparseable or permanently refused, and neither leaves a
    /// `messages` row for triage to load.
    pub added: Vec<String>,
    pub relabelled: usize,
    pub deleted: usize,
    /// Label changes for messages outside the cached window. Counted, not
    /// fetched: re-reading fifty messages in the Gmail UI would cost the whole
    /// per-second quota budget in `UNREAD` flips.
    pub label_changes_for_uncached: usize,
    pub threads: usize,
    pub parse_failures: usize,
    /// Messages a page named that Gmail permanently refuses to serve. Audited
    /// and accounted, exactly as the full sync accounts for them, so one dead
    /// message cannot pin the cursor forever.
    pub fetch_failures: usize,
    /// History entries carrying no message id. See [`PagePlan::malformed`].
    pub malformed_records: usize,
    /// The walk looped because a notification arrived while it was running.
    pub reruns: usize,
    /// The drain limit was hit with a notification still outstanding, so a
    /// successor job is owed.
    pub handed_over: bool,
    pub cursor_before: Option<i64>,
    pub cursor_after: Option<i64>,
    /// The stored cursor was too old for Gmail to serve; a reconciliation is
    /// owed. The caller runs it - see [`super::sweep`].
    pub cursor_expired: bool,
    /// No cursor at all: nothing has ever fully synced, so a full window is
    /// owed instead of a walk.
    pub needs_full_sync: bool,
    pub paused: bool,
    pub skipped_concurrent: bool,
}

/// Walk `users.history.list` from the stored cursor and apply every page.
///
/// # Errors
/// Returns an error if a page's touched messages could not all be accounted
/// for - in which case that page's cursor is deliberately **not** advanced -
/// or if a database or transport call fails.
pub async fn run_incremental(
    pool: &PgPool,
    client: &GmailClient,
    account_id: Uuid,
    options: &SyncOptions,
) -> Result<IncrementalReport> {
    let mut report = IncrementalReport::default();

    let status: Option<String> = sqlx::query_scalar("select status from accounts where id = $1")
        .bind(account_id)
        .fetch_optional(pool)
        .await?;
    match status.as_deref() {
        None => {
            report.paused = true;
            return Ok(report);
        }
        Some("needs_reauth") => {
            // Identical to the full sync's rule: no Gmail call at all.
            report.paused = true;
            return Ok(report);
        }
        Some(_) => {}
    }

    let Some(lock) = AccountLock::take(pool, account_id).await? else {
        report.skipped_concurrent = true;
        return Ok(report);
    };
    let outcome = walk(pool, client, account_id, options, &mut report).await;
    lock.release().await;
    outcome?;
    Ok(report)
}

async fn walk(
    pool: &PgPool,
    client: &GmailClient,
    account_id: Uuid,
    options: &SyncOptions,
    report: &mut IncrementalReport,
) -> Result<()> {
    let cursor: Option<i64> =
        sqlx::query_scalar("select last_history_id from sync_state where account_id = $1")
            .bind(account_id)
            .fetch_optional(pool)
            .await?
            .flatten();
    report.cursor_before = cursor;
    report.cursor_after = cursor;

    let Some(mut walk_start) = cursor else {
        // Nothing has ever synced, so there is no point from which "what
        // changed" is even a question.
        report.needs_full_sync = true;
        return Ok(());
    };

    // Every thread the walk touched, across all pages - counted for
    // `report.threads` and nothing else. Rollups are refreshed per page.
    let mut threads: BTreeSet<String> = BTreeSet::new();

    // The outer loop is the drain. After the last page, the cursor is re-read
    // and the walk restarts if new history arrived while we were working -
    // which is what stops a push that lands mid-walk from waiting for the
    // 30-minute poll.
    loop {
        let mut page_token: Option<String> = None;

        loop {
            let page = match client.list_history(walk_start, page_token.as_deref()).await {
                Ok(page) => page,
                Err(crate::gmail::client::GmailError::NotFound) => {
                    // The cursor aged out of Gmail's log.
                    //
                    // The debt is recorded **before** anything else, and only a
                    // completed sweep clears it. Recording it afterwards - or
                    // relying on the 404 happening again - loses the
                    // reconciliation for good: the recovery re-sync commits a
                    // fresh cursor, so the retry gets a `200` instead of the
                    // `404` that enters recovery, and nothing ever reconciles.
                    let mut tx = pool.begin().await?;
                    store::owe_reconcile(&mut tx, account_id, Some(walk_start)).await?;
                    tx.commit().await?;
                    report.cursor_expired = true;
                    return Ok(());
                }
                Err(error) => {
                    return Err(anyhow::Error::new(error).context("walking Gmail history"))
                }
            };
            report.pages += 1;

            let plan = plan_page(&page);
            report.records += plan.records;

            // Network first, transaction second.
            let fetched = fetch_added(client, options, &plan.to_fetch).await?;

            // Rollups per page, exactly as the full sync refreshes per batch:
            // refreshing the cumulative set would re-run every earlier page's
            // threads on every later page, turning a long catch-up quadratic.
            // The cumulative `threads` set survives only to count for
            // `report.threads`.
            let mut page_threads: BTreeSet<String> = BTreeSet::new();

            let mut tx = pool.begin().await?;
            for row in &fetched.rows {
                store::upsert_message(&mut tx, account_id, row).await?;
                page_threads.insert(row.thread_id.clone());
                report.added.push(row.gmail_id.clone());
            }

            // **The mail trigger, committed with the mail that fires it.**
            //
            // `fetched.rows`, not `plan.to_fetch`: the plan is what this page
            // *asked Gmail for*, and it includes the ids that came back
            // unparseable (`fetched.parse_failures`) or permanently refused
            // (`fetched.permanent_failures`). Neither leaves a `messages` row,
            // so triaging them would enqueue a job per dead message that can
            // only ever load nothing. That set is exactly what the loop above
            // pushed onto `report.added`, whose own doc comment calls it "the
            // gmail ids `messagesAdded` produced **and we ingested**".
            //
            // One statement for the page, not one per row: this transaction is
            // holding its locks while it runs, and Gmail's default history page
            // is a hundred records.
            let triaged: Vec<String> = fetched
                .rows
                .iter()
                .map(|row| row.gmail_id.clone())
                .collect();
            crate::agents::triage::enqueue_many_in(&mut *tx, account_id, &triaged).await?;

            for (gmail_id, error) in &fetched.parse_failures {
                report.parse_failures += 1;
                store::audit_in(
                    &mut tx,
                    account_id,
                    "message_parse_failed",
                    json!({ "gmail_id": gmail_id, "error": error, "source": "history" }),
                )
                .await?;
            }
            for (gmail_id, status, detail) in &fetched.permanent_failures {
                report.fetch_failures += 1;
                store::audit_in(
                    &mut tx,
                    account_id,
                    "message_fetch_failed",
                    json!({ "gmail_id": gmail_id, "status": status, "detail": detail,
                            "source": "history" }),
                )
                .await?;
            }
            if plan.malformed > 0 {
                report.malformed_records += plan.malformed;
                store::audit_in(
                    &mut tx,
                    account_id,
                    "history_record_malformed",
                    json!({ "count": plan.malformed, "cursor": plan.cursor }),
                )
                .await?;
            }
            for (gmail_id, (added, removed)) in &plan.label_deltas {
                match store::apply_label_delta(&mut tx, account_id, gmail_id, added, removed)
                    .await?
                {
                    Some(thread_id) => {
                        report.relabelled += 1;
                        page_threads.insert(thread_id);
                    }
                    None => report.label_changes_for_uncached += 1,
                }
            }
            for gmail_id in &plan.deleted {
                if let Some(thread_id) =
                    store::soft_delete_message(&mut tx, account_id, gmail_id).await?
                {
                    report.deleted += 1;
                    page_threads.insert(thread_id);
                }
            }
            for thread_id in &page_threads {
                store::refresh_thread(&mut tx, account_id, thread_id).await?;
            }
            threads.extend(page_threads);

            match plan.cursor {
                Some(cursor) => {
                    store::advance_history_id(&mut tx, account_id, Some(cursor)).await?;
                    report.cursor_after = Some(cursor);
                }
                // An empty page is a successful check that changed nothing.
                None => store::touch_checked(&mut tx, account_id).await?,
            }
            tx.commit().await?;

            match page.next_page_token {
                Some(token) => page_token = Some(token),
                None => break,
            }
        }

        // Drain. Two separate questions, and only asking both closes the race.
        //
        // The first is cheap: did *this* walk advance the cursor? If it did,
        // restart from where it got to - Gmail's history may have grown while
        // we were reading it.
        //
        // The second is the one the cursor cannot answer. A notification that
        // landed after the last (empty) page and before this job finishes was
        // suppressed at enqueue by the dedupe index, and the webhook never
        // writes the notification's own `historyId`, so `last_history_id` looks
        // exactly as it did before. The suppressed enqueue records itself on
        // `sync_state.rerun_requested` instead; taking and clearing that flag
        // in one statement is what stops the wakeup being lost.
        let mut tx = pool.begin().await?;
        let rerun_requested = store::take_rerun_request(&mut tx, account_id).await?;
        let latest: Option<i64> =
            sqlx::query_scalar("select last_history_id from sync_state where account_id = $1")
                .bind(account_id)
                .fetch_optional(&mut *tx)
                .await?
                .flatten();
        tx.commit().await?;

        // Only a real signal justifies another pass.
        //
        // "Did our own cursor move?" is not one: it moved because *we* moved
        // it, so it would be true after every productive walk and would spend
        // an extra `history.list` confirming silence, for ever, with no reason
        // to think anything had arrived.
        if !rerun_requested {
            break;
        }
        report.reruns += 1;

        if report.reruns >= MAX_DRAIN_PASSES {
            // Put the request back rather than dropping it, and say so in the
            // report so the caller can enqueue the successor. Restoring the flag
            // alone would leave a real notification represented by nothing but a
            // boolean, waiting on the 30-minute poll.
            store::request_rerun(pool, account_id).await?;
            report.handed_over = true;
            tracing::info!(
                reruns = report.reruns,
                "handing the walk back after the drain limit"
            );
            break;
        }

        // Resume from wherever the walk actually got to.
        if let Some(latest) = latest {
            walk_start = latest;
        }
    }

    report.threads = threads.len();
    Ok(())
}

/// What a page's `messagesAdded` fetch produced.
#[derive(Debug, Default)]
struct Fetched {
    rows: Vec<store::IngestRow>,
    /// `(gmail_id, error)` for messages that parsed badly. Stored as
    /// metadata-only rows and audited **inside the page transaction**, so the
    /// evidence commits with the page rather than surviving its rollback.
    parse_failures: Vec<(String, String)>,
    /// `(gmail_id, status, detail)` for messages Gmail will never serve.
    permanent_failures: Vec<(String, u16, String)>,
}

/// Batch-fetch the messages a page added. Network only - no transaction is
/// open, and nothing is written.
///
/// A **transient** failure is an error for the whole page: advancing past a
/// record whose message we never stored is how mail goes missing with nothing
/// to notice, and asking again is likely to work. A **permanent** failure is
/// not - asking again produces the same 400 forever, so it is audited and
/// accounted exactly as the full sync accounts for it (`sync_window` STEP 6),
/// or one dead message pins incremental sync for the life of the account. A
/// message Gmail no longer has is neither: it was deleted between the record
/// and the fetch, which is ordinary.
async fn fetch_added(
    client: &GmailClient,
    options: &SyncOptions,
    ids: &[String],
) -> Result<Fetched> {
    let mut fetched = Fetched::default();
    // EDGE (empty input): a page of pure label changes fetches nothing, and
    // must not start a pacer or issue a request.
    if ids.is_empty() {
        return Ok(fetched);
    }

    let mut transient: Vec<String> = Vec::new();
    // The same one-batch-per-interval pacing the full sync uses. The token
    // bucket alone does not provide it: a full bucket funds 250 units and a
    // ten-message batch costs 50, so five batches would leave back to back.
    let mut pacer = Pacer::new(options.batch_interval);

    for chunk in ids.chunks(options.batch_size) {
        pacer.wait().await;
        let outcomes = client
            .batch_get_raw(chunk)
            .await
            .context("fetching messages a history page added")?;
        for outcome in outcomes {
            match outcome {
                BatchOutcome::Fetched {
                    gmail_id,
                    message,
                    raw,
                } => match parse::parse(&raw, &gmail_id) {
                    Ok(parsed) => fetched
                        .rows
                        .push(store::IngestRow::parsed(&message, parsed)),
                    Err(error) => {
                        tracing::warn!(%gmail_id, %error, "unparseable message; storing metadata only");
                        fetched
                            .parse_failures
                            .push((gmail_id.clone(), error.to_string()));
                        fetched.rows.push(store::IngestRow::metadata_only(&message));
                    }
                },
                BatchOutcome::Gone { gmail_id } => {
                    tracing::debug!(%gmail_id, "history named a message Gmail no longer has");
                }
                BatchOutcome::Failed {
                    gmail_id,
                    status,
                    detail,
                    transient: is_transient,
                } => {
                    if is_transient {
                        tracing::debug!(%gmail_id, status, "transient fetch failure; the page will be re-read");
                        transient.push(gmail_id);
                    } else {
                        tracing::warn!(%gmail_id, status, %detail, "permanently could not fetch a message a history page added");
                        fetched.permanent_failures.push((gmail_id, status, detail));
                    }
                }
            }
        }
    }

    if !transient.is_empty() {
        anyhow::bail!(
            "{} of {} messages this history page added could not be fetched right now ({}); \
             the page's cursor stays where it is so the next run re-reads it",
            transient.len(),
            ids.len(),
            json!(transient)
        );
    }
    Ok(fetched)
}

/// The `gmail_incremental` handler.
///
/// It does three things in order, and the order is the point: an outstanding
/// reconciliation is settled **first**, because a walk from a cursor we already
/// know is untrustworthy would just advance past the drift; then the walk; then
/// recovery, if the walk found the cursor expired.
pub struct IncrementalHandler {
    state: AppState,
}

impl IncrementalHandler {
    /// Ready to hand to [`crate::jobs::Registry::register`].
    #[must_use]
    pub fn shared(state: AppState) -> Arc<dyn Handler> {
        Arc::new(Self { state })
    }
}

impl std::fmt::Debug for IncrementalHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("IncrementalHandler")
    }
}

#[async_trait]
impl Handler for IncrementalHandler {
    async fn handle(&self, job: Job, _ctx: JobContext) -> Result<()> {
        // A payload without an account - a legacy or hand-inserted row - can
        // only mean the sole account; with several there is no honest guess.
        let account_id = match job.payload.get("account_id").and_then(|v| v.as_str()) {
            Some(raw) => Uuid::parse_str(raw).context("the account_id in the job payload")?,
            None => match self.state.sole_account().await? {
                Some(account) => account.id,
                None => {
                    tracing::info!("no sole Gmail account to fall back to; nothing to walk");
                    return Ok(());
                }
            },
        };

        let client = self.state.gmail.client_for(account_id);
        let options = SyncOptions::from_config(&self.state.config);

        let report = settle_and_walk(&self.state.pool, &client, account_id, &options).await?;

        // **A busy lock is not a completed notification.** The house convention
        // returns `Ok(skipped_concurrent)`, and an `Ok` handler is marked done -
        // so a push racing a full sync would be acknowledged and its work thrown
        // away, blowing the 60-second target with nothing to show for it.
        //
        // One bounded hop: re-enqueue ten seconds out, with a flag so a second
        // skip stops rather than looping. The running sync has almost certainly
        // covered the change by then, and the poll is the backstop either way.
        if report.skipped_concurrent {
            let requeued = job
                .payload
                .get("requeued")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if !requeued {
                let queue = Queue::new(self.state.pool.clone(), self.state.config.jobs.clone());
                queue
                    .enqueue(
                        KIND,
                        &json!({ "account_id": account_id, "requeued": true }),
                        Some(Utc::now() + chrono::Duration::seconds(10)),
                    )
                    .await?;
            } else {
                tracing::info!("a second lock contention; leaving it to the poll");
            }
        }
        Ok(())
    }
}

/// Settle any outstanding reconciliation, walk, and reconcile if the walk found
/// the cursor expired. Separated from the handler so tests can drive it without
/// a queue.
///
/// # Errors
/// Returns an error if the walk or the reconciliation fails.
pub async fn settle_and_walk(
    pool: &PgPool,
    client: &GmailClient,
    account_id: Uuid,
    options: &SyncOptions,
) -> Result<IncrementalReport> {
    if let Some(stale) = store::reconcile_owed(pool, account_id).await? {
        reconcile_under_lock(pool, client, account_id, options, Some(stale)).await?;
    }

    let report = run_incremental(pool, client, account_id, options).await?;

    if report.cursor_expired {
        let stale = store::reconcile_owed(pool, account_id).await?;
        reconcile_under_lock(pool, client, account_id, options, stale).await?;
    }
    Ok(report)
}

/// Enqueue a walk, collapsing a burst into one pending job.
///
/// # Errors
/// Returns an error if the enqueue fails.
pub async fn enqueue(queue: &Queue, account_id: Uuid) -> sqlx::Result<Option<i64>> {
    queue
        .enqueue_unique(
            KIND,
            &json!({ "account_id": account_id }),
            None,
            &format!("{KIND}:{account_id}"),
        )
        .await
}

/// Take the account lock and reconcile. A busy lock is not an error: whoever
/// holds it is doing this work, and the debt survives in `reconcile_after`
/// either way.
async fn reconcile_under_lock(
    pool: &PgPool,
    client: &GmailClient,
    account_id: Uuid,
    options: &SyncOptions,
    stale_cursor: Option<i64>,
) -> Result<()> {
    let Some(lock) = AccountLock::take(pool, account_id).await? else {
        tracing::info!("another worker holds the account lock; the reconciliation debt stands");
        return Ok(());
    };
    let outcome = super::sweep::reconcile(pool, client, account_id, options, stale_cursor).await;
    lock.release().await;
    outcome?;
    Ok(())
}

#[cfg(test)]
mod tests;
