//! `expire_approvals`: the sweep that ages an unanswered card out.
//!
//! `API.md` §7: "Approvals expire **7 days** after creation. A cron sweeps
//! expired ones hourly."
//!
//! # Why a probe and not a timer
//!
//! `sync::schedule` already establishes the doctrine: there is no cron in this
//! server, `run_after` is the only scheduling primitive the queue has, and
//! due-ness is a question for PostgreSQL rather than for a wall clock a test
//! cannot move. The ticker asks one indexed `exists` — `feed_items_expiry_idx`
//! covers the whole predicate — and enqueues a single deduplicated job only
//! when something is actually overdue. A quiet server pays one boolean per
//! tick, and a test drives [`sweep`] directly by writing a deadline in the
//! past.
//!
//! # Why it is not part of `gmail_maintenance`
//!
//! That job is per account and its ticker skips accounts in `needs_reauth`. An
//! approval does not stop being seven days old because Gmail's token died, and
//! a card that could never expire would hold `new_count` above zero for ever.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;

use crate::{
    api::feed,
    jobs::{Handler, Job, JobContext, Queue},
    state::AppState,
};

/// The job kind. `lib.rs::register_handlers` reserves the name.
pub const KIND: &str = "expire_approvals";

/// One pending sweep at a time, for the whole server.
pub const DEDUPE_KEY: &str = "expire_approvals";

/// How many cards one job settles before handing the rest to the next.
///
/// `for update skip locked` means a second worker can take the next batch
/// concurrently, and the ticker re-enqueues while anything is left, so this is
/// a bound on one transaction rather than on the backlog.
const BATCH: i64 = 200;

/// Is anything overdue?
///
/// # Errors
/// Returns an error if the query fails.
pub async fn due(pool: &sqlx::PgPool) -> Result<bool> {
    let due: bool = sqlx::query_scalar(
        "select exists ( \
             select 1 from feed_items \
              where kind = 'approval' and status = 'new' and approval_expires_at <= now())",
    )
    .fetch_one(pool)
    .await?;
    Ok(due)
}

/// Put one sweep on the queue, if there is not one there already.
///
/// # Errors
/// Returns an error if the insert fails.
pub async fn enqueue(queue: &Queue) -> Result<Option<i64>> {
    Ok(queue
        .enqueue_unique(KIND, &json!({}), None, DEDUPE_KEY)
        .await?)
}

/// Age out everything overdue. Returns how many cards it settled.
///
/// Each card is its **own transaction**: a batch of two hundred that failed on
/// the last one would otherwise roll back a hundred and ninety-nine correct
/// expiries, and there is no reason for them to share a fate. The card, its run
/// and the resume job move together — that pair is what has to be atomic.
///
/// # Errors
/// Returns an error if the database is unreachable.
pub async fn sweep(state: &AppState) -> Result<usize> {
    let mut settled = 0;
    loop {
        // The ids first, and **without a lock**. Holding two hundred row locks
        // for the length of the batch would mean one failing card rolls back
        // the other hundred and ninety-nine; the job would then retry into the
        // same wall five times and dead-letter, after which **nothing ever
        // expires again**. That is not theoretical: `expire_card` calls
        // `resume::enqueue_in`, which `ensure!`s. The listing is a hint, and
        // `claim_expired_one` below takes the real lock and re-checks — so a
        // second sweep seeing the same ids costs a re-read and nothing else.
        let ids = feed::claim_expired(&state.pool, BATCH).await?;
        if ids.is_empty() {
            break;
        }
        let taken = ids.len();

        for id in ids {
            // One card, one transaction: the card, its run and the resume job
            // move together, and that pair is the only thing that has to be
            // atomic.
            let mut tx = state.pool.begin().await?;
            let Some(card) = feed::claim_expired_one(&mut tx, id).await? else {
                // Somebody answered it between the listing and now, which is
                // exactly what the re-read is for.
                tx.rollback().await?;
                continue;
            };
            let (account_id, card_id) = (card.account_id, card.id);
            feed::expire_card(&mut tx, &card).await?;
            // In the transaction, not through `agents::audit`: the expiry and
            // its record commit together or not at all, which is the one place
            // a best-effort audit row would be wrong.
            sqlx::query(
                "insert into audit_log (account_id, actor, action, subject) \
                 values ($1, 'system', 'feed.expire', $2)",
            )
            .bind(account_id)
            .bind(json!({ "feed_item_id": card_id }))
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            settled += 1;
        }

        if taken < usize::try_from(BATCH).unwrap_or(usize::MAX) {
            break;
        }
    }
    Ok(settled)
}

pub struct ExpireApprovalsHandler {
    state: AppState,
}

impl ExpireApprovalsHandler {
    #[must_use]
    pub fn shared(state: AppState) -> Arc<dyn Handler> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Handler for ExpireApprovalsHandler {
    async fn handle(&self, _job: Job, _ctx: JobContext) -> Result<()> {
        let settled = sweep(&self.state).await?;
        if settled > 0 {
            tracing::info!(settled, "expired unanswered approvals");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
