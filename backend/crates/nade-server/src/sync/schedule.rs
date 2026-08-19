//! Watch renewal and the polling fallback.
//!
//! There is no cron in this server, no leader election, and `run_after` is the
//! only scheduling primitive the queue has. Two obvious mechanisms both lose:
//!
//! A **self-rescheduling job** keeps its chain in the `jobs` table, and the
//! chain can *die*: five failures dead-letters the row and nothing recreates
//! it, so half an hour of Gmail being unreachable would silently end mail
//! freshness for ever - in the phase called "mail stays current".
//!
//! A **`tokio::interval` in `main::serve`** is unreachable from
//! `cargo test -p nade-server`, because `main.rs` is a separate binary target;
//! and a 24-hour interval in a process restarted every hour never fires at all.
//!
//! So: a ticker that lives in the library, is a pure enqueuer, and asks
//! PostgreSQL what is overdue. Due-ness is one statement over `sync_state`, so
//! tests never touch a wall clock - they set state with SQL and call [`due`]
//! directly, and every timestamp is computed by the database, which is the
//! doctrine `jobs.rs` already follows.
//!
//! The watch registration lives here too rather than in a module of its own:
//! it is one Gmail call and one upsert, and its only caller is the maintenance
//! job below.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use super::SyncOptions;
use crate::{
    config::ScheduleConfig,
    gmail::client::GmailClient,
    jobs::{Handler, Job, JobContext, Queue},
    state::AppState,
};

/// The job kind that does both overdue things.
pub const KIND: &str = "gmail_maintenance";

/// One pending maintenance job at a time, whatever asked for it.
pub const DEDUPE_KEY: &str = "gmail_maintenance";

/// What the database says is overdue.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Due {
    pub watch: bool,
    pub poll: bool,
}

impl Due {
    #[must_use]
    pub const fn nothing(self) -> bool {
        !self.watch && !self.poll
    }
}

/// Ask PostgreSQL what is overdue for this account.
///
/// One round trip, and the authority for both the ticker's "should I enqueue?"
/// and the handler's "should I act?" - so the two can never disagree.
///
/// The join on `accounts.status` is load-bearing. A `needs_reauth` account
/// never advances its cursor, so a poll-due test that ignored status would say
/// "poll" for ever and enqueue a job every tick that immediately does nothing.
///
/// # Errors
/// Returns an error if the query fails.
pub async fn due(
    pool: &PgPool,
    account_id: Uuid,
    config: &ScheduleConfig,
    topic: Option<&str>,
) -> Result<Due> {
    let watch_hours = i64::try_from(config.watch_renew_after.as_secs() / 3600).unwrap_or(24);
    let poll_minutes = i64::try_from(config.poll_after.as_secs() / 60).unwrap_or(30);

    // A watch with no topic can never be registered, so it can never stop being
    // due - and the ticker would enqueue a job every tick that deliberately
    // does nothing. `$4 is not null` is what makes "poll only" actually poll
    // only, which is the documented dev-without-a-tunnel mode.
    let row: Option<(bool, bool)> = sqlx::query_as(
        "select \
             $4 is not null and coalesce(s.watch_renewed_at is null \
                   or s.watch_renewed_at < now() - make_interval(hours => $2::int) \
                   or s.watch_expiry <= now() + interval '1 day' \
                   or s.watch_topic is distinct from $4, true) as watch, \
             coalesce(s.last_checked_at is null \
                   or s.last_checked_at < now() - make_interval(mins => $3::int), true) as poll \
           from accounts a left join sync_state s on s.account_id = a.id \
          where a.id = $1 and a.status <> 'needs_reauth'",
    )
    .bind(account_id)
    .bind(i32::try_from(watch_hours).unwrap_or(24))
    .bind(i32::try_from(poll_minutes).unwrap_or(30))
    .bind(topic)
    .fetch_optional(pool)
    .await?;

    // No row means no account, or one that needs re-consent. Nothing is due for
    // either, and saying so here is what stops the ticker spinning.
    Ok(row.map_or_else(Due::default, |(watch, poll)| Due { watch, poll }))
}

/// Register (or re-register) the Gmail watch.
///
/// `users.watch` **replaces** any existing registration rather than adding one,
/// so renewal has no window in which a notification could be lost.
///
/// # Errors
/// Returns an error if Gmail refuses or the write fails.
pub async fn renew_watch(
    pool: &PgPool,
    client: &GmailClient,
    account_id: Uuid,
    topic: &str,
) -> Result<()> {
    let registration = client
        .watch(topic, &[])
        .await
        .context("registering the Gmail watch")?;

    sqlx::query(
        "insert into sync_state (account_id, watch_expiry, watch_renewed_at, watch_topic, updated_at) \
         values ($1, $2, now(), $3, now()) \
         on conflict (account_id) do update set \
             watch_expiry = excluded.watch_expiry, \
             watch_renewed_at = now(), \
             watch_topic = excluded.watch_topic, \
             updated_at = now()",
    )
    .bind(account_id)
    .bind(registration.expires_at())
    .bind(topic)
    .execute(pool)
    .await?;

    // The registration's own historyId is NOT stored as a cursor. It describes
    // the mailbox now, not a point we have walked to, and writing it would skip
    // everything between the last walk and this renewal.
    tracing::info!(
        expires_at = ?registration.expires_at(),
        "gmail watch registered"
    );
    Ok(())
}

/// What one maintenance run did.
#[derive(Debug, Default, serde::Serialize)]
pub struct MaintenanceReport {
    pub watch_renewed: bool,
    pub polled: bool,
    /// Both steps are attempted even when the first fails, so a Gmail outage on
    /// `users.watch` cannot also stop the poll that keeps mail current.
    pub errors: Vec<String>,
}

/// Do whatever [`due`] says.
///
/// # Errors
/// Returns an error only if the due-ness query itself fails. A failure in
/// either step is collected into the report and re-raised at the end, so the
/// job still retries but the other step has already run.
pub async fn run_maintenance(state: &AppState, account_id: Uuid) -> Result<MaintenanceReport> {
    let mut report = MaintenanceReport::default();
    let due = due(
        &state.pool,
        account_id,
        &state.config.schedule,
        state.config.push.topic.as_deref(),
    )
    .await?;
    if due.nothing() {
        return Ok(report);
    }

    let client = state.gmail.client_for(account_id);
    let options = SyncOptions::from_config(&state.config);

    if due.watch {
        match state.config.push.topic.as_deref() {
            Some(topic) => match renew_watch(&state.pool, &client, account_id, topic).await {
                Ok(()) => report.watch_renewed = true,
                Err(error) => {
                    tracing::warn!(%error, "the gmail watch could not be renewed");
                    report.errors.push(format!("watch: {error}"));
                }
            },
            // No topic configured is the dev-without-a-tunnel case, not a
            // failure. The poll below is what keeps mail current there.
            None => tracing::debug!("no push topic configured; skipping the watch renewal"),
        }
    }

    if due.poll {
        match super::incremental::settle_and_walk(&state.pool, &client, account_id, &options).await
        {
            Ok(walk) => {
                report.polled = true;
                if walk.handed_over {
                    // The walk stopped at its drain limit with a notification
                    // still outstanding. Hand it to a fresh job rather than
                    // leaving it to the poll.
                    let queue = Queue::new(state.pool.clone(), state.config.jobs.clone());
                    let _ = super::incremental::enqueue(&queue, account_id).await?;
                }
                if walk.needs_full_sync {
                    // Nothing has ever synced, so there is no cursor to walk
                    // from. Hand over to the full sync rather than looping.
                    let queue = Queue::new(state.pool.clone(), state.config.jobs.clone());
                    super::enqueue(&queue, account_id).await?;
                }
            }
            Err(error) => {
                tracing::warn!(%error, "the polling fallback failed");
                report.errors.push(format!("poll: {error}"));
            }
        }
    }

    if let Some(first) = report.errors.first() {
        anyhow::bail!("maintenance finished with errors: {first}");
    }
    Ok(report)
}

/// Ask for a maintenance run, unless one is already waiting.
///
/// # Errors
/// Returns an error if the enqueue fails.
pub async fn enqueue(queue: &Queue, account_id: Uuid) -> sqlx::Result<Option<i64>> {
    queue
        .enqueue_unique(KIND, &json!({ "account_id": account_id }), None, DEDUPE_KEY)
        .await
}

/// Start the ticker.
///
/// It is a **pure enqueuer**: it asks the database what is overdue and, if
/// anything is, puts one deduplicated job on the queue. It never talks to
/// Gmail and never writes anything but that job, so a tick costs one query on a
/// quiet server.
///
/// Returns a handle the caller aborts at shutdown. It lives here rather than in
/// `main::serve` so it is reachable from `cargo test -p nade-server`; a
/// `tokio::interval` in the binary target would be untestable by construction.
pub fn spawn(state: AppState, queue: Queue) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(state.config.schedule.tick);
        // The first tick fires immediately, which is what makes a restart
        // re-evaluate instead of waiting out a whole 24-hour cadence.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(error) = tick_once(&state, &queue).await {
                // A failing tick must not end the loop: the next one is the
                // recovery, and there is nothing else to restart it.
                tracing::warn!(%error, "a scheduler tick failed");
            }
        }
    })
}

/// One tick. Separated so a test can drive it without a timer.
///
/// # Errors
/// Returns an error if the account lookup, the due-ness query, or the enqueue
/// fails.
pub async fn tick_once(state: &AppState, queue: &Queue) -> Result<bool> {
    let Some(account) = state.account().await? else {
        return Ok(false);
    };
    let due = due(
        &state.pool,
        account.id,
        &state.config.schedule,
        state.config.push.topic.as_deref(),
    )
    .await?;
    if due.nothing() {
        return Ok(false);
    }
    Ok(enqueue(queue, account.id).await?.is_some())
}

/// The `gmail_maintenance` handler.
pub struct MaintenanceHandler {
    state: AppState,
}

impl MaintenanceHandler {
    /// Ready to hand to [`crate::jobs::Registry::register`].
    #[must_use]
    pub fn shared(state: AppState) -> Arc<dyn Handler> {
        Arc::new(Self { state })
    }
}

impl std::fmt::Debug for MaintenanceHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MaintenanceHandler")
    }
}

#[async_trait]
impl Handler for MaintenanceHandler {
    async fn handle(&self, job: Job, _ctx: JobContext) -> Result<()> {
        let account_id = match job.payload.get("account_id").and_then(|v| v.as_str()) {
            Some(raw) => Uuid::parse_str(raw).context("the account_id in the job payload")?,
            None => match self.state.account().await? {
                Some(account) => account.id,
                None => return Ok(()),
            },
        };
        run_maintenance(&self.state, account_id).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
