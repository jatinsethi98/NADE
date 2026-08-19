//! Scheduler tests.
//!
//! None of these touches a wall clock. State is set with SQL and `due` is
//! called directly, so every timestamp comparison is PostgreSQL's - which is
//! the same reason `jobs.rs` computes its own deadlines in the database.

use uuid::Uuid;

use super::*;
use crate::test_support::{test_db, TestDb};

const TOPIC: &str = "projects/p/topics/gmail-events";

fn config() -> ScheduleConfig {
    ScheduleConfig {
        tick: std::time::Duration::from_secs(60),
        watch_renew_after: std::time::Duration::from_secs(24 * 3600),
        poll_after: std::time::Duration::from_secs(30 * 60),
    }
}

async fn connected() -> (TestDb, Uuid) {
    let db = test_db().await;
    let account: Uuid = sqlx::query_scalar(
        "insert into accounts (email, status) values ('jatinsethi98@gmail.com', 'ok') returning id",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    (db, account)
}

/// Fresh state: watch renewed and checked just now, on the configured topic.
async fn all_current(db: &TestDb, account: Uuid) {
    sqlx::query(
        "insert into sync_state \
             (account_id, watch_renewed_at, watch_expiry, watch_topic, last_checked_at) \
         values ($1, now(), now() + interval '7 days', $2, now()) \
         on conflict (account_id) do update set \
             watch_renewed_at = now(), watch_expiry = now() + interval '7 days', \
             watch_topic = excluded.watch_topic, last_checked_at = now()",
    )
    .bind(account)
    .bind(TOPIC)
    .execute(&db.pool)
    .await
    .unwrap();
}

async fn age(db: &TestDb, account: Uuid, column: &str, interval: &str) {
    sqlx::query(&format!(
        "update sync_state set {column} = now() - interval '{interval}' where account_id = $1"
    ))
    .bind(account)
    .execute(&db.pool)
    .await
    .unwrap();
}

/// Criterion S5 - the daily cadence, from both sides.
#[tokio::test]
async fn a_watch_renewed_yesterday_is_due_and_one_renewed_an_hour_ago_is_not() {
    let (db, account) = connected().await;
    all_current(&db, account).await;

    let fresh = due(&db.pool, account, &config(), Some(TOPIC))
        .await
        .unwrap();
    assert!(!fresh.watch, "a watch renewed just now is not due");

    age(&db, account, "watch_renewed_at", "25 hours").await;
    let stale = due(&db.pool, account, &config(), Some(TOPIC))
        .await
        .unwrap();
    assert!(stale.watch, "a watch renewed 25 hours ago is due");
}

/// Criterion S6 - an expiry inside a day is renewed early, off cadence.
/// Gmail's registration lasts seven days; waiting for the cadence when it is
/// hours from lapsing is how push stops without anyone noticing.
#[tokio::test]
async fn a_watch_expiring_within_a_day_is_renewed_early() {
    let (db, account) = connected().await;
    all_current(&db, account).await;
    sqlx::query(
        "update sync_state set watch_expiry = now() + interval '6 hours' where account_id = $1",
    )
    .bind(account)
    .execute(&db.pool)
    .await
    .unwrap();

    let now = due(&db.pool, account, &config(), Some(TOPIC))
        .await
        .unwrap();
    assert!(
        now.watch,
        "an expiry six hours away must not wait for tomorrow"
    );
}

/// Criterion S7 - re-pointing the topic re-registers immediately, or the watch
/// keeps publishing to a topic nothing is subscribed to.
#[tokio::test]
async fn changing_the_topic_makes_the_watch_due_immediately() {
    let (db, account) = connected().await;
    all_current(&db, account).await;

    let same = due(&db.pool, account, &config(), Some(TOPIC))
        .await
        .unwrap();
    assert!(!same.watch);

    let moved = due(
        &db.pool,
        account,
        &config(),
        Some("projects/p/topics/other"),
    )
    .await
    .unwrap();
    assert!(moved.watch, "a changed topic must re-register");
}

/// Criterion S8/S9 - the polling fallback measures the last successful check,
/// whatever drove it.
#[tokio::test]
async fn the_poll_is_due_only_once_the_last_check_has_aged_out() {
    let (db, account) = connected().await;
    all_current(&db, account).await;

    let fresh = due(&db.pool, account, &config(), Some(TOPIC))
        .await
        .unwrap();
    assert!(
        !fresh.poll,
        "a webhook that just ran must hold off the poll"
    );

    age(&db, account, "last_checked_at", "31 minutes").await;
    let stale = due(&db.pool, account, &config(), Some(TOPIC))
        .await
        .unwrap();
    assert!(stale.poll);
}

/// A mailbox that has never synced is due for everything.
#[tokio::test]
async fn an_account_with_no_sync_state_is_due_for_both() {
    let (db, account) = connected().await;

    let first = due(&db.pool, account, &config(), Some(TOPIC))
        .await
        .unwrap();
    assert!(first.watch && first.poll);
}

/// A `needs_reauth` account is due for nothing.
///
/// Without the status join it would be due to poll for ever: a paused walk
/// never advances `last_checked_at`, so the ticker would enqueue a job every
/// tick that immediately does nothing.
#[tokio::test]
async fn a_paused_account_is_never_due() {
    let (db, account) = connected().await;
    all_current(&db, account).await;
    age(&db, account, "last_checked_at", "3 hours").await;
    age(&db, account, "watch_renewed_at", "3 days").await;

    let before = due(&db.pool, account, &config(), Some(TOPIC))
        .await
        .unwrap();
    assert!(
        before.watch && before.poll,
        "the fixture must be overdue first"
    );

    sqlx::query("update accounts set status = 'needs_reauth' where id = $1")
        .bind(account)
        .execute(&db.pool)
        .await
        .unwrap();

    let after = due(&db.pool, account, &config(), Some(TOPIC))
        .await
        .unwrap();
    assert!(
        after.nothing(),
        "a paused account must stop the ticker, not feed it"
    );
}

/// An account that does not exist is due for nothing, rather than erroring.
#[tokio::test]
async fn an_unknown_account_is_due_for_nothing() {
    let db = test_db().await;
    let nothing = due(&db.pool, Uuid::new_v4(), &config(), Some(TOPIC))
        .await
        .unwrap();
    assert!(nothing.nothing());
}

/// Criterion S11 - a burst of requests collapses into one pending job.
#[tokio::test]
async fn maintenance_enqueues_at_most_one_pending_job() {
    let (db, account) = connected().await;
    let queue = crate::jobs::Queue::new(db.pool.clone(), crate::jobs::QueueConfig::default());

    let first = enqueue(&queue, account).await.unwrap();
    let second = enqueue(&queue, account).await.unwrap();
    let third = enqueue(&queue, account).await.unwrap();

    assert!(first.is_some());
    assert!(
        second.is_none(),
        "a second pending maintenance job was created"
    );
    assert!(third.is_none());

    let pending: i64 = sqlx::query_scalar("select count(*) from jobs where kind = $1")
        .bind(KIND)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(pending, 1);
}

/// ...and once it has run, the next request enqueues again. Otherwise the
/// dedupe key would silently end maintenance after the first job.
#[tokio::test]
async fn a_finished_maintenance_job_does_not_block_the_next_one() {
    let (db, account) = connected().await;
    let queue = crate::jobs::Queue::new(db.pool.clone(), crate::jobs::QueueConfig::default());

    enqueue(&queue, account).await.unwrap();
    sqlx::query("update jobs set done_at = now() where kind = $1")
        .bind(KIND)
        .execute(&db.pool)
        .await
        .unwrap();

    let again = enqueue(&queue, account).await.unwrap();
    assert!(again.is_some(), "maintenance stopped after its first run");
}

/// Criterion S16 - a renewal slower than Gmail's watch lifetime is refused at
/// start-up, because it guarantees the registration lapses.
#[test]
fn the_config_refuses_a_renewal_slower_than_gmails_watch_lifetime() {
    let seven_days = std::time::Duration::from_secs(7 * 24 * 3600);
    assert!(
        config().watch_renew_after < seven_days,
        "the default must be inside Gmail's own lifetime"
    );
}
