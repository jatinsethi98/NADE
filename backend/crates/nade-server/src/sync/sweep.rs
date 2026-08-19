//! Recovery from a history cursor Gmail will no longer serve.
//!
//! Gmail keeps its history log for a bounded time. A mailbox left alone for
//! long enough - or a server that was off - comes back to a `404`, meaning
//! "your cursor is older than anything I still have". The cursor is useless and
//! there is no way to learn what changed while we were away.
//!
//! So the window is re-synced, and then **reconciled**: anything inside the
//! window that the fresh listing did not mention has gone, and our row is the
//! only thing that still believes otherwise.
//!
//! The dangerous half is the reconciliation, because "not in the listing" and
//! "the listing did not finish" look identical from here.

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use super::{store, SyncOptions, SyncReport};
use crate::gmail::client::GmailClient;

/// What a reconciliation did.
#[derive(Debug, Default, serde::Serialize)]
pub struct SweepReport {
    /// Ids the fresh listing enumerated.
    pub listed: usize,
    /// The listing hit `MAX_SYNC_MESSAGES` and is therefore only the newest
    /// part of the window.
    pub truncated: bool,
    /// Nothing older than this was considered.
    pub floor: Option<DateTime<Utc>>,
    pub swept: usize,
    pub threads_rebuilt: usize,
}

/// Re-sync the window and reconcile it against what we hold.
///
/// The caller **must** already hold the account lock.
///
/// # Errors
/// Returns an error if the re-sync could not account for every message, or if a
/// database call fails. The sweep does not run in that case - see below.
pub async fn reconcile(
    pool: &PgPool,
    client: &GmailClient,
    account_id: Uuid,
    options: &SyncOptions,
    stale_cursor: Option<i64>,
) -> Result<SweepReport> {
    store::audit(
        pool,
        account_id,
        "gmail_history_expired",
        json!({ "stale_cursor": stale_cursor }),
    )
    .await?;

    let mut sync = SyncReport::default();
    let mut listed_ids: Vec<String> = Vec::new();
    // If this fails, so does the whole recovery, and the sweep never runs.
    //
    // That conditional is the single most important property here: **the delete
    // set is derived from a listing we were able to fully account for**, because
    // a listing that gave up half way through looks exactly like "these
    // messages were deleted". A 429 storm mid-listing would otherwise
    // soft-delete the mailbox.
    super::sync_window(
        pool,
        client,
        account_id,
        options,
        &mut sync,
        &mut listed_ids,
    )
    .await?;

    sweep(
        pool,
        account_id,
        options,
        &listed_ids,
        sync.listed,
        sync.listing_truncated,
    )
    .await
}

/// The reconciliation proper: soft-delete everything inside the window that the
/// fresh listing did not mention, and rebuild the threads that lost messages.
async fn sweep(
    pool: &PgPool,
    account_id: Uuid,
    options: &SyncOptions,
    listed_ids: &[String],
    listed: usize,
    listed_truncated: bool,
) -> Result<SweepReport> {
    // Whether the listing was cut short is the *listing's* answer, not a
    // count comparison: a mailbox holding exactly the cap with no next page is
    // complete, and treating it as truncated narrows the floor to its oldest
    // live message, leaving genuinely deleted rows behind while the debt is
    // cleared anyway.
    let truncated = listed_truncated;

    // GUARD 1 - the window floor.
    //
    // `messages` legitimately holds rows from outside the sync window: every
    // search hit is cached (`mail::cache::fetch_missing`), and opening an old
    // thread pulls in the whole conversation. A `newer_than:30d` listing will
    // never mention any of them, so without a floor the sweep deletes every
    // message the user ever searched for.
    //
    // GUARD 2 - the cap.
    //
    // `list_message_ids` truncates at `MAX_SYNC_MESSAGES`, so a full window may
    // not have been enumerated at all. `messages.list` is newest-first, which
    // means a truncated listing enumerates exactly the *suffix* of the window -
    // so the floor becomes the oldest message we actually saw, not the window's
    // own edge. Not the later of the two: in the untruncated case we want
    // `now - window`, because the band between it and the oldest listed message
    // is precisely where genuinely deleted mail lives.
    let floor: Option<DateTime<Utc>> = if truncated {
        sqlx::query_scalar(
            "select min(internal_ts) from messages \
              where account_id = $1 and gmail_id = any($2)",
        )
        .bind(account_id)
        .bind(listed_ids)
        .fetch_one(pool)
        .await?
    } else {
        Some(Utc::now() - Duration::days(i64::from(options.window_days)))
    };

    let mut report = SweepReport {
        listed,
        truncated,
        floor,
        ..SweepReport::default()
    };

    let Some(floor) = floor else {
        // A truncated listing with nothing stored to date it. There is no
        // defensible floor, so there is no defensible delete set.
        tracing::warn!("no floor could be computed; skipping the sweep rather than guessing");
        return Ok(report);
    };

    let mut tx = pool.begin().await?;

    // GUARD 3 - `internal_ts is null` never matches `>= floor`, so a
    // metadata-only row for a message Gmail gave no `internalDate` survives.
    // That is the safe direction, and it is a property of the SQL rather than a
    // rule someone has to remember.
    // `>` for a truncated listing, `>=` otherwise, and the difference is a
    // message that still exists.
    //
    // A capped listing's floor is the oldest message it returned. Gmail pages
    // by `internalDate`, so the cap can fall **inside** a group of messages
    // sharing that exact timestamp - the listing returns some of them and stops.
    // With `>=`, the ones it did not reach match the predicate, are absent from
    // `listed_ids`, and are soft-deleted while still live in Gmail. `>` keeps
    // the whole boundary cohort, at the cost of leaving any genuinely deleted
    // message at that one timestamp for the next reconciliation.
    let boundary = if truncated {
        "internal_ts > $2"
    } else {
        "internal_ts >= $2"
    };
    let gone: Vec<(String, String)> = sqlx::query_as(&format!(
        "update messages set deleted_at = now() \
          where account_id = $1 \
            and deleted_at is null \
            and {boundary} \
            and not (gmail_id = any($3)) \
         returning gmail_id, thread_id"
    ))
    .bind(account_id)
    .bind(floor)
    .bind(listed_ids)
    .fetch_all(&mut *tx)
    .await?;

    report.swept = gone.len();

    // "Per-label membership rebuild" needs no new code: `refresh_thread`
    // recomputes the rollup from live messages only, rebuilds `thread_labels`
    // from their union, and deletes the thread row when nothing live is left.
    let mut threads: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (_, thread_id) in &gone {
        threads.insert(thread_id.clone());
    }
    for thread_id in &threads {
        store::refresh_thread(&mut tx, account_id, thread_id).await?;
    }
    report.threads_rebuilt = threads.len();

    // The reconciliation is owed no longer.
    store::clear_reconcile(&mut tx, account_id).await?;
    store::audit_in(
        &mut tx,
        account_id,
        "gmail_reconciled",
        json!({
            "listed": report.listed,
            "truncated": report.truncated,
            "floor": report.floor,
            "swept": report.swept,
            "threads_rebuilt": report.threads_rebuilt,
        }),
    )
    .await?;
    tx.commit().await?;

    Ok(report)
}

#[cfg(test)]
mod tests;
