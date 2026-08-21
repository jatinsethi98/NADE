//! Reconciliation tests.
//!
//! Every one of these is about a way the sweep could delete mail that is still
//! there. That is its only failure mode, and it is silent.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, Request, ResponseTemplate,
};

use super::*;
use crate::{
    gmail::{
        client::{Endpoints, GmailClient},
        oauth::StaticTokens,
        quota::Bucket,
    },
    test_support::{test_db, TestDb},
};

/// A Gmail whose 30-day listing is exactly `ids`.
async fn gmail_listing(ids: &[&str]) -> MockServer {
    gmail_listing_with(ids, false).await
}

/// ...and one that says it has more to give, which is what truncation really
/// is. `listed == cap` is not: a mailbox holding exactly the cap with no next
/// page is complete.
async fn gmail_listing_truncated(ids: &[&str]) -> MockServer {
    gmail_listing_with(ids, true).await
}

async fn gmail_listing_with(ids: &[&str], has_more: bool) -> MockServer {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/profile"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"emailAddress":"jatinsethi98@gmail.com","historyId":"9412771"}"#.to_vec(),
            "application/json",
        ))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/labels"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"labels":[{"id":"INBOX","name":"INBOX","type":"system"}]}"#.to_vec(),
            "application/json",
        ))
        .mount(&server)
        .await;

    let listed: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .respond_with(move |_: &Request| {
            let entries: Vec<String> = listed
                .iter()
                .map(|id| format!(r#"{{"id":"{id}","threadId":"t-{id}"}}"#))
                .collect();
            let token = if has_more {
                r#","nextPageToken":"more""#
            } else {
                ""
            };
            ResponseTemplate::new(200).set_body_raw(
                format!(r#"{{"messages":[{}]{token}}}"#, entries.join(",")).into_bytes(),
                "application/json",
            )
        })
        .mount(&server)
        .await;

    // The re-sync fetches everything it lists - it does not consult the cache
    // first - so the batch endpoint has to answer for real, or the whole
    // recovery fails and the sweep (correctly) never runs.
    let fixtures: Vec<crate::sync::tests::Fixture> = ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            let mut fixture = crate::sync::tests::Fixture::new(index);
            fixture.id = (*id).to_owned();
            fixture.thread_id = format!("t-{id}");
            fixture
        })
        .collect();
    Mock::given(method("POST"))
        .and(path("/batch/gmail/v1"))
        .respond_with(move |request: &Request| crate::sync::tests::batch_reply(request, &fixtures))
        .mount(&server)
        .await;

    server
}

fn client_for(server: &MockServer) -> GmailClient {
    GmailClient::new(
        crate::gmail::http_client().unwrap(),
        Endpoints::at(&server.uri()),
        Arc::new(Bucket::new()),
        Arc::new(StaticTokens("ya29.test".to_owned())),
    )
    .with_retry_budget(2, 0.0)
}

fn options(max_messages: usize) -> SyncOptions {
    SyncOptions {
        query: "newer_than:30d".to_owned(),
        window_days: 30,
        max_messages,
        batch_size: crate::gmail::client::MAX_BATCH,
        batch_interval: std::time::Duration::from_millis(1),
        max_retry_rounds: super::super::MAX_RETRY_ROUNDS,
        retry_backoff: std::time::Duration::from_millis(1),
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

/// A stored message of a given age, plus its thread rollup.
async fn seed(db: &TestDb, account: Uuid, gmail_id: &str, days_old: i64) {
    sqlx::query(
        "insert into messages (account_id, gmail_id, thread_id, internal_ts, label_ids, body_text) \
         values ($1, $2, $3, $4, '{INBOX}', '')",
    )
    .bind(account)
    .bind(gmail_id)
    .bind(format!("t-{gmail_id}"))
    .bind(Utc::now() - Duration::days(days_old))
    .execute(&db.pool)
    .await
    .unwrap();

    let mut tx = db.pool.begin().await.unwrap();
    store::refresh_thread(&mut tx, account, &format!("t-{gmail_id}"))
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn is_live(db: &TestDb, account: Uuid, gmail_id: &str) -> bool {
    sqlx::query_scalar::<_, bool>(
        "select deleted_at is null from messages where account_id = $1 and gmail_id = $2",
    )
    .bind(account)
    .bind(gmail_id)
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

/// Criterion R1 - a message inside the window that the fresh listing does not
/// mention has gone, and the sweep is what notices.
#[tokio::test]
async fn a_message_missing_from_the_fresh_listing_is_swept() {
    let (db, account) = connected().await;
    seed(&db, account, "kept", 5).await;
    seed(&db, account, "gone", 5).await;

    let server = gmail_listing(&["kept"]).await;
    let report = reconcile(
        &db.pool,
        &client_for(&server),
        account,
        &options(2_000),
        Some(1),
    )
    .await
    .unwrap();

    assert_eq!(report.swept, 1);
    assert!(is_live(&db, account, "kept").await);
    assert!(!is_live(&db, account, "gone").await);
}

/// **The guard that matters most.** `messages` legitimately holds mail from
/// outside the window: every search hit is cached, and opening an old thread
/// pulls in the whole conversation. A `newer_than:30d` listing never mentions
/// any of it, so a sweep without a floor deletes everything the user ever
/// searched for.
#[tokio::test]
async fn the_sweep_never_touches_a_message_older_than_the_window() {
    let (db, account) = connected().await;
    seed(&db, account, "recent", 5).await;
    // Cached by a search, 400 days ago. A 30-day listing cannot name it.
    seed(&db, account, "ancient", 400).await;

    let server = gmail_listing(&["recent"]).await;
    let report = reconcile(
        &db.pool,
        &client_for(&server),
        account,
        &options(2_000),
        Some(1),
    )
    .await
    .unwrap();

    assert_eq!(report.swept, 0, "a cached search hit was deleted");
    assert!(is_live(&db, account, "ancient").await);
}

/// Criterion R3 - a truncated listing enumerates only the newest part of the
/// window, so the floor becomes the oldest message actually seen rather than
/// the window's own edge.
#[tokio::test]
async fn a_truncated_listing_narrows_the_floor_to_what_it_actually_saw() {
    let (db, account) = connected().await;
    seed(&db, account, "newest", 1).await;
    // Inside 30 days, but older than anything the capped listing returned - so
    // it is beyond the horizon, not deleted.
    seed(&db, account, "older", 20).await;

    // Truncated because Gmail still had a page token when we stopped - not
    // because the count happened to equal the cap.
    let server = gmail_listing_truncated(&["newest"]).await;
    let report = reconcile(
        &db.pool,
        &client_for(&server),
        account,
        &options(1),
        Some(1),
    )
    .await
    .unwrap();

    assert!(report.truncated);
    assert_eq!(
        report.swept, 0,
        "the sweep reached past what the listing covered"
    );
    assert!(is_live(&db, account, "older").await);
}

/// Criterion R5 - a metadata-only row Gmail gave no `internalDate` fails
/// `>= floor` and survives. The safe direction, and a property of the SQL.
#[tokio::test]
async fn a_message_with_no_timestamp_is_never_swept() {
    let (db, account) = connected().await;
    seed(&db, account, "dated", 5).await;
    sqlx::query(
        "insert into messages (account_id, gmail_id, thread_id, internal_ts, label_ids, body_text) \
         values ($1, 'undated', 't-undated', null, '{INBOX}', '')",
    )
    .bind(account)
    .execute(&db.pool)
    .await
    .unwrap();

    let server = gmail_listing(&["dated"]).await;
    let report = reconcile(
        &db.pool,
        &client_for(&server),
        account,
        &options(2_000),
        Some(1),
    )
    .await
    .unwrap();

    assert_eq!(report.swept, 0);
    assert!(is_live(&db, account, "undated").await);
}

/// Criterion R6 - a thread whose last live message is swept loses its row, so
/// the mail list never shows an empty conversation.
#[tokio::test]
async fn a_thread_whose_last_message_is_swept_disappears() {
    let (db, account) = connected().await;
    seed(&db, account, "gone", 5).await;

    let before: i64 = sqlx::query_scalar("select count(*) from threads where account_id = $1")
        .bind(account)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(before, 1, "the fixture must start with a thread row");

    let server = gmail_listing(&[]).await;
    reconcile(
        &db.pool,
        &client_for(&server),
        account,
        &options(2_000),
        Some(1),
    )
    .await
    .unwrap();

    let after: i64 = sqlx::query_scalar("select count(*) from threads where account_id = $1")
        .bind(account)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(after, 0, "an empty thread row outlived its messages");
}

/// Criterion R9 - sweeping an already-swept mailbox changes nothing.
#[tokio::test]
async fn sweeping_twice_changes_nothing_the_second_time() {
    let (db, account) = connected().await;
    seed(&db, account, "kept", 5).await;
    seed(&db, account, "gone", 5).await;

    let server = gmail_listing(&["kept"]).await;
    let first = reconcile(
        &db.pool,
        &client_for(&server),
        account,
        &options(2_000),
        Some(1),
    )
    .await
    .unwrap();
    let second = reconcile(
        &db.pool,
        &client_for(&server),
        account,
        &options(2_000),
        Some(1),
    )
    .await
    .unwrap();

    assert_eq!(first.swept, 1);
    assert_eq!(
        second.swept, 0,
        "the second sweep found work that was already done"
    );
}

/// The debt is cleared only by a completed sweep. That is what makes the
/// reconciliation survive a crash between the re-sync and the sweep, which is
/// otherwise unreachable: the re-sync commits a fresh cursor, so the retry gets
/// a `200` instead of the `404` that enters recovery.
#[tokio::test]
async fn a_completed_sweep_clears_the_reconciliation_debt() {
    let (db, account) = connected().await;
    seed(&db, account, "kept", 5).await;

    let mut tx = db.pool.begin().await.unwrap();
    store::owe_reconcile(&mut tx, account, Some(42))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        store::reconcile_owed(&db.pool, account).await.unwrap(),
        Some(42),
        "the debt must be readable before the sweep"
    );

    let server = gmail_listing(&["kept"]).await;
    reconcile(
        &db.pool,
        &client_for(&server),
        account,
        &options(2_000),
        Some(42),
    )
    .await
    .unwrap();

    assert_eq!(
        store::reconcile_owed(&db.pool, account).await.unwrap(),
        None,
        "a completed sweep must clear the debt"
    );
}

/// **The tie at the floor.**
///
/// A capped listing stops wherever the cap falls, and Gmail pages by
/// `internalDate` - so the cut can land *inside* a group of messages sharing
/// one timestamp. With a `>=` boundary the ones the listing did not reach match
/// the predicate, are absent from `listed_ids`, and are soft-deleted while
/// still live in Gmail. This is the test for the `>`.
#[tokio::test]
async fn a_truncated_listing_never_sweeps_a_message_tied_with_its_floor() {
    let (db, account) = connected().await;

    // Two messages at exactly the same instant; the capped listing returns one.
    //
    // **The instant is the fixture's own**, not `now() - 3 days`. The re-sync
    // fetches everything it lists and writes the fixture's `internalDate` over
    // the cached row, so a wall-clock timestamp here meant the two rows were
    // never actually tied - the listed one silently moved to the fixture's
    // instant while only the *unlisted* one kept `now() - 3d`. Whether the test
    // passed then depended on which side of 2026-08-16T09:12:04Z the clock
    // happened to be: green until 2026-08-19T09:12:04Z and failing after, and
    // asserting nothing about ties in either state. Reading the instant from
    // `Fixture` is what makes the tie real and the test independent of the day
    // it runs on.
    let shared = DateTime::from_timestamp_millis(crate::sync::tests::Fixture::new(0).internal_ms)
        .expect("the fixture's internalDate is a valid instant");
    for id in ["listed", "tied"] {
        sqlx::query(
            "insert into messages (account_id, gmail_id, thread_id, internal_ts, label_ids, body_text) \
             values ($1, $2, $3, $4, '{INBOX}', '')",
        )
        .bind(account)
        .bind(id)
        .bind(format!("t-{id}"))
        .bind(shared)
        .execute(&db.pool)
        .await
        .unwrap();
        let mut tx = db.pool.begin().await.unwrap();
        store::refresh_thread(&mut tx, account, &format!("t-{id}"))
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    let server = gmail_listing_truncated(&["listed"]).await;
    let report = reconcile(
        &db.pool,
        &client_for(&server),
        account,
        &options(1),
        Some(1),
    )
    .await
    .unwrap();

    assert!(report.truncated);
    assert_eq!(
        report.swept, 0,
        "a live message sharing the floor's timestamp was deleted"
    );
    assert!(is_live(&db, account, "tied").await);
}

/// A mailbox of exactly the cap with **no** next page is complete, not
/// truncated - so the floor stays at the window edge and genuinely deleted mail
/// is still reconciled.
#[tokio::test]
async fn a_full_page_with_no_next_token_is_not_treated_as_truncated() {
    let (db, account) = connected().await;
    seed(&db, account, "kept", 5).await;
    seed(&db, account, "gone", 5).await;

    // The listing returns one id and offers no more; the cap is 1.
    let server = gmail_listing(&["kept"]).await;
    let report = reconcile(
        &db.pool,
        &client_for(&server),
        account,
        &options(1),
        Some(1),
    )
    .await
    .unwrap();

    assert!(!report.truncated, "a complete listing was called truncated");
    assert_eq!(
        report.swept, 1,
        "a deleted message survived a complete listing"
    );
    assert!(!is_live(&db, account, "gone").await);
}
