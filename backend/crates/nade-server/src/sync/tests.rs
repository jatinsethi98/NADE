//! Sync tests. Every Gmail call goes through `wiremock`; nothing here touches
//! Google.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use base64::Engine as _;
use chrono::{DateTime, Utc};
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

/// One message the fake Gmail will serve.
#[derive(Debug, Clone)]
struct Fixture {
    id: String,
    thread_id: String,
    labels: Vec<String>,
    internal_ms: i64,
    raw: Vec<u8>,
    /// Answer this sub-request with a 404, as if the user had just deleted it.
    gone: bool,
    /// Answer with a 500, as if Gmail hiccuped on this one message. Transient.
    broken: bool,
    /// Answer with a 400. Permanent: asking again produces the same 400.
    permanent: bool,
    /// Answer with the **verbatim** 429 the live account returned, this many
    /// times, then serve the message. Shared across clones so the count
    /// survives into the mock closure.
    throttle_first: usize,
    throttled: Arc<AtomicUsize>,
}

impl Fixture {
    fn new(index: usize) -> Self {
        Self {
            id: format!("msg{index:04}"),
            thread_id: format!("thr{index:04}"),
            labels: vec!["INBOX".to_owned(), "CATEGORY_PERSONAL".to_owned()],
            // 2026-08-16T09:12:04Z, one second apart per index.
            internal_ms: 1_786_950_724_000 + (index as i64) * 1000,
            raw: rfc822(
                &format!("Sender {index} <sender{index}@example.com>"),
                &format!("Subject number {index}"),
                &format!("Body of message {index}."),
            ),
            gone: false,
            broken: false,
            permanent: false,
            throttle_first: 0,
            throttled: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn with_thread(mut self, thread_id: &str) -> Self {
        self.thread_id = thread_id.to_owned();
        self
    }

    fn with_labels(mut self, labels: &[&str]) -> Self {
        self.labels = labels.iter().map(|l| (*l).to_owned()).collect();
        self
    }

    fn with_raw(mut self, raw: Vec<u8>) -> Self {
        self.raw = raw;
        self
    }

    fn at(mut self, millis: i64) -> Self {
        self.internal_ms = millis;
        self
    }

    fn gone(mut self) -> Self {
        self.gone = true;
        self
    }

    fn broken(mut self) -> Self {
        self.broken = true;
        self
    }

    fn permanently_broken(mut self) -> Self {
        self.permanent = true;
        self
    }

    /// Throttled for `times` requests, then served - a real rate limit, which
    /// is temporary by definition.
    fn throttled_for(mut self, times: usize) -> Self {
        self.throttle_first = times;
        self
    }
}

/// Byte-for-byte what Gmail returned inside a `200` batch response 91 times
/// during the first live sync. Copied from the `audit_log` rather than invented,
/// because the exact `reason` is what the classifier keys on.
const LIVE_429: &str = r#"{"error":{"code":429,"message":"Too many concurrent requests for user.",
  "errors":[{"message":"Too many concurrent requests for user.","domain":"global",
             "reason":"rateLimitExceeded"}],"status":"RESOURCE_EXHAUSTED"}}"#;

fn rfc822(from: &str, subject: &str, body: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: jatinsethi98@gmail.com\r\nSubject: {subject}\r\n\
         Date: Sun, 16 Aug 2026 09:12:04 +0000\r\nContent-Type: text/plain; charset=\"utf-8\"\r\n\r\n{body}\r\n"
    )
    .into_bytes()
}

/// A fake Gmail: profile, labels, list and the batch endpoint.
struct FakeGmail {
    server: MockServer,
}

impl FakeGmail {
    async fn start(fixtures: Vec<Fixture>) -> Self {
        let server = MockServer::start().await;
        // The cursor every test asserts against.
        let history_id = "9412771";

        Mock::given(method("GET"))
            .and(path("/gmail/v1/users/me/profile"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"emailAddress":"jatinsethi98@gmail.com","messagesTotal":63120,"historyId":"{history_id}"}}"#
                )
                .into_bytes(),
                "application/json",
            ))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/gmail/v1/users/me/labels"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(
                    br#"{"labels":[
                     {"id":"INBOX","name":"INBOX","type":"system"},
                     {"id":"UNREAD","name":"UNREAD","type":"system"},
                     {"id":"CATEGORY_PERSONAL","name":"CATEGORY_PERSONAL","type":"system"},
                     {"id":"SENT","name":"SENT","type":"system"},
                     {"id":"Label_12","name":"To Reply","type":"user"},
                     {"id":"Label_99","name":"[Gmail]All Mail","type":"user"}]}"#
                        .to_vec(),
                    "application/json",
                ),
            )
            .mount(&server)
            .await;

        let listed = fixtures.clone();
        Mock::given(method("GET"))
            .and(path("/gmail/v1/users/me/messages"))
            .respond_with(move |request: &Request| {
                let want: usize = request
                    .url
                    .query_pairs()
                    .find(|(key, _)| key == "maxResults")
                    .and_then(|(_, value)| value.parse().ok())
                    .unwrap_or(500);
                let entries: Vec<String> = listed
                    .iter()
                    .take(want)
                    .map(|fixture| {
                        format!(
                            r#"{{"id":"{}","threadId":"{}"}}"#,
                            fixture.id, fixture.thread_id
                        )
                    })
                    .collect();
                ResponseTemplate::new(200).set_body_raw(
                    format!(r#"{{"messages":[{}]}}"#, entries.join(",")).into_bytes(),
                    "application/json",
                )
            })
            .mount(&server)
            .await;

        let batched = fixtures;
        Mock::given(method("POST"))
            .and(path("/batch/gmail/v1"))
            .respond_with(move |request: &Request| batch_reply(request, &batched))
            .mount(&server)
            .await;

        Self { server }
    }

    fn client(&self) -> GmailClient {
        GmailClient::new(
            crate::gmail::http_client().unwrap(),
            Endpoints::at(&self.server.uri()),
            Arc::new(Bucket::new()),
            Arc::new(StaticTokens("ya29.test".to_owned())),
        )
        .with_retry_budget(2, 0.0)
    }
}

/// Answer a real multipart batch, **in reverse order**, so every sync test also
/// re-proves that correlation is by `Content-ID` and not by position.
fn batch_reply(request: &Request, fixtures: &[Fixture]) -> ResponseTemplate {
    let body = String::from_utf8_lossy(&request.body).into_owned();
    let mut asked: Vec<(String, String)> = Vec::new();
    let mut content_id: Option<String> = None;

    for line in body.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("Content-ID: <") {
            content_id = Some(rest.trim_end_matches('>').to_owned());
        } else if let Some(rest) = line.strip_prefix("GET /gmail/v1/users/me/messages/") {
            let id = rest.split('?').next().unwrap_or_default().to_owned();
            if let Some(cid) = content_id.take() {
                asked.push((cid, id));
            }
        }
    }

    let mut out = String::new();
    for (cid, id) in asked.iter().rev() {
        let fixture = fixtures.iter().find(|fixture| &fixture.id == id);
        let (status, payload) = match fixture {
            None | Some(Fixture { gone: true, .. }) => (
                404,
                r#"{"error":{"code":404,"message":"Requested entity was not found."}}"#.to_owned(),
            ),
            Some(Fixture { broken: true, .. }) => (
                500,
                r#"{"error":{"code":500,"message":"Backend Error"}}"#.to_owned(),
            ),
            Some(Fixture {
                permanent: true, ..
            }) => (
                400,
                r#"{"error":{"code":400,"message":"Invalid id value","errors":[
                     {"domain":"global","reason":"invalidArgument"}]}}"#
                    .to_owned(),
            ),
            Some(fixture)
                if fixture.throttle_first > 0
                    && fixture.throttled.fetch_add(1, Ordering::SeqCst)
                        < fixture.throttle_first =>
            {
                (429, LIVE_429.to_owned())
            }
            Some(fixture) => {
                let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&fixture.raw);
                let labels: Vec<String> = fixture
                    .labels
                    .iter()
                    .map(|label| format!("\"{label}\""))
                    .collect();
                (
                    200,
                    format!(
                        r#"{{"id":"{}","threadId":"{}","labelIds":[{}],"historyId":"9412770",
                             "internalDate":"{}","snippet":"a &amp; b","raw":"{raw}"}}"#,
                        fixture.id,
                        fixture.thread_id,
                        labels.join(","),
                        fixture.internal_ms
                    ),
                )
            }
        };
        out.push_str(&format!(
            "--bnd\r\nContent-Type: application/http\r\nContent-ID: <response-{cid}>\r\n\r\n\
             HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\n\r\n{payload}\r\n"
        ));
    }
    out.push_str("--bnd--\r\n");

    ResponseTemplate::new(200).set_body_raw(out.into_bytes(), "multipart/mixed; boundary=bnd")
}

/// A database with one connected account.
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

fn fast_options() -> SyncOptions {
    SyncOptions {
        query: "newer_than:30d".to_owned(),
        max_messages: 2_000,
        batch_size: crate::gmail::client::MAX_BATCH,
        // The pacer is a real 1 s in production; the tests assert that
        // separately in `the_dev_caps_are_applied` rather than sleeping here.
        batch_interval: std::time::Duration::from_millis(1),
        max_retry_rounds: MAX_RETRY_ROUNDS,
        // The retry *schedule* is asserted in `retry_delay_is_exponential`;
        // the sync tests assert the outcome, and must not sleep for 30 s to
        // do it.
        retry_backoff: std::time::Duration::from_millis(1),
    }
}

// ------------------------------------------------------------- the tests --

/// Criterion N1 - the ordering that turns a gap into an overlap.
#[tokio::test]
async fn history_id_is_read_before_listing() {
    let fixtures: Vec<Fixture> = (0..3).map(Fixture::new).collect();
    let gmail = FakeGmail::start(fixtures).await;
    let (db, account) = connected().await;

    let report = run_sync(&db.pool, &gmail.client(), account, &fast_options())
        .await
        .unwrap();

    let calls: Vec<String> = gmail
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect();

    let profile_at = calls
        .iter()
        .position(|p| p.ends_with("/profile"))
        .expect("the profile must be read");
    let list_at = calls
        .iter()
        .position(|p| p.ends_with("/messages"))
        .expect("the list must happen");
    assert!(
        profile_at < list_at,
        "historyId must be read BEFORE listing, or a message arriving mid-sync \
         falls into a gap: {calls:?}"
    );

    assert_eq!(report.history_id, Some(9_412_771));
    let stored: Option<i64> =
        sqlx::query_scalar("select last_history_id from sync_state where account_id = $1")
            .bind(account)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        stored,
        Some(9_412_771),
        "the cursor recorded is the one read before the listing"
    );
}

/// Criterion N2 - dev caps are law, and they are applied by the code rather than
/// hoped for.
#[tokio::test]
async fn the_dev_caps_are_applied() {
    // The production values, asserted where they are decided.
    let mut config = crate::config::tests::sample();
    config.gmail.sync_window_days = 30;
    config.gmail.max_sync_messages = 2_000;
    config.gmail.batch_size = 45;
    let production = SyncOptions::from_config(&config);
    assert_eq!(production.query, "newer_than:30d");
    assert_eq!(production.max_messages, 2_000);
    assert_eq!(production.batch_size, 45);
    assert_eq!(
        production.batch_interval,
        std::time::Duration::from_secs(1),
        "at most one batch per second"
    );

    // And the query really goes on the wire, with the cap really applied.
    let fixtures: Vec<Fixture> = (0..10).map(Fixture::new).collect();
    let gmail = FakeGmail::start(fixtures).await;
    let (db, account) = connected().await;

    let options = SyncOptions {
        max_messages: 4,
        ..fast_options()
    };
    let report = run_sync(&db.pool, &gmail.client(), account, &options)
        .await
        .unwrap();
    assert_eq!(report.listed, 4);
    assert_eq!(report.ingested, 4);

    let list_call = gmail
        .server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|request| request.url.path().ends_with("/messages"))
        .expect("a list call");
    let query: std::collections::HashMap<String, String> = list_call
        .url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    assert_eq!(query["q"], "newer_than:30d");
    assert_eq!(query["maxResults"], "4");

    let stored: i64 = sqlx::query_scalar("select count(*) from messages where account_id = $1")
        .bind(account)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(stored, 4, "the cap is a hard stop, not a suggestion");
}

/// Criterion N3, restated against the batch width the live sync forced.
#[tokio::test]
async fn batches_are_capped_and_paced() {
    let width = crate::gmail::client::MAX_BATCH;
    let fixtures: Vec<Fixture> = (0..width * 2).map(Fixture::new).collect();
    let gmail = FakeGmail::start(fixtures).await;
    let (db, account) = connected().await;

    let report = run_sync(&db.pool, &gmail.client(), account, &fast_options())
        .await
        .unwrap();
    assert_eq!(report.listed, width * 2);
    assert_eq!(report.ingested, width * 2);
    assert_eq!(
        report.batches, 2,
        "twice the batch width is exactly two batches"
    );

    let sizes: Vec<usize> = gmail
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path().ends_with("/batch/gmail/v1"))
        .map(|request| {
            String::from_utf8_lossy(&request.body)
                .matches("Content-ID: <item-")
                .count()
        })
        .collect();
    assert_eq!(sizes, vec![width, width]);
    assert!(
        width <= crate::gmail::quota::MAX_CONCURRENT_SUBREQUESTS,
        "a batch is `n` concurrent requests to Google; 45 produced 429s live"
    );

    // Real pacing, measured once at a scale that does not slow the suite down.
    let paced = std::time::Instant::now();
    let mut pacer = Pacer::new(std::time::Duration::from_millis(120));
    pacer.wait().await;
    pacer.wait().await;
    assert!(
        paced.elapsed() >= std::time::Duration::from_millis(110),
        "the pacer must actually wait between batches"
    );
}

/// Criterion N4 + mandated edge case 9 (**partial failure mid-sync**).
#[tokio::test]
async fn a_parse_failure_is_recorded_and_the_sync_continues() {
    let mut fixtures: Vec<Fixture> = (0..5).map(Fixture::new).collect();
    // Bytes that are not a message at all.
    fixtures[2] = fixtures[2].clone().with_raw(Vec::new());

    let gmail = FakeGmail::start(fixtures).await;
    let (db, account) = connected().await;

    let report = run_sync(&db.pool, &gmail.client(), account, &fast_options())
        .await
        .unwrap();

    assert_eq!(report.ingested, 5, "every message still gets a row");
    assert_eq!(report.parse_failures, 1);

    // The unparseable one is a metadata-only row: no body, but the thread,
    // labels and timestamp Gmail gave us are all there.
    let (body_text, body_html, thread_id, labels, ts): (
        String,
        Option<String>,
        String,
        Vec<String>,
        Option<DateTime<Utc>>,
    ) = sqlx::query_as(
        "select body_text, body_html, thread_id, label_ids, internal_ts \
           from messages where account_id = $1 and gmail_id = 'msg0002'",
    )
    .bind(account)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(body_text, "", "body_text is never null, even when empty");
    assert_eq!(body_html, None);
    assert_eq!(thread_id, "thr0002");
    assert_eq!(labels, vec!["INBOX", "CATEGORY_PERSONAL"]);
    assert!(ts.is_some(), "internalDate survives a parse failure");

    // And the failure is on the record.
    let audited: i64 = sqlx::query_scalar(
        "select count(*) from audit_log \
          where action = 'message_parse_failed' and subject->>'gmail_id' = 'msg0002'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(audited, 1);

    // The other four are fully parsed.
    let bodies: i64 = sqlx::query_scalar(
        "select count(*) from messages where account_id = $1 and body_text <> ''",
    )
    .bind(account)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(bodies, 4);
}

/// Criterion N5 + mandated edge case 10 (**a message that vanishes between list
/// and get**).
#[tokio::test]
async fn a_message_that_vanished_is_skipped() {
    let mut fixtures: Vec<Fixture> = (0..5).map(Fixture::new).collect();
    fixtures[3] = fixtures[3].clone().gone();

    let gmail = FakeGmail::start(fixtures).await;
    let (db, account) = connected().await;

    let report = run_sync(&db.pool, &gmail.client(), account, &fast_options())
        .await
        .unwrap();

    assert_eq!(report.vanished, 1);
    assert_eq!(report.ingested, 4, "the other four still land");
    assert_eq!(report.fetch_failures, 0, "a deletion is not a failure");

    let rows: i64 = sqlx::query_scalar(
        "select count(*) from messages where account_id = $1 and gmail_id = 'msg0003'",
    )
    .bind(account)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(rows, 0);

    // Exactly one batch: a 404 sub-response must not trigger a retry of the
    // whole batch.
    let batches = gmail
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path().ends_with("/batch/gmail/v1"))
        .count();
    assert_eq!(batches, 1, "a vanished message must never be retried");

    // And nothing was audited as a failure, because nothing failed.
    let noise: i64 = sqlx::query_scalar(
        "select count(*) from audit_log where action in ('message_fetch_failed', 'message_parse_failed')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(noise, 0);
}

/// A message Gmail will **never** serve is counted, audited, and left behind -
/// it does not take the batch with it, and it does not hold the cursor either,
/// because asking again cannot change the answer.
#[tokio::test]
async fn one_permanently_broken_message_does_not_stop_the_batch() {
    let mut fixtures: Vec<Fixture> = (0..5).map(Fixture::new).collect();
    fixtures[1] = fixtures[1].clone().permanently_broken();

    let gmail = FakeGmail::start(fixtures).await;
    let (db, account) = connected().await;

    let report = run_sync(&db.pool, &gmail.client(), account, &fast_options())
        .await
        .expect("a permanent failure is not a reason to fail the sync");
    assert_eq!(report.ingested, 4);
    assert_eq!(report.fetch_failures, 1);
    assert_eq!(
        report.unresolved, 0,
        "a 400 is resolved: it will never work"
    );
    assert_eq!(report.retry_rounds, 0, "a 400 must not be retried at all");

    let audited: i64 = sqlx::query_scalar(
        "select count(*) from audit_log \
          where action = 'message_fetch_failed' and subject->>'gmail_id' = 'msg0001'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(audited, 1);

    // The cursor moves, because nothing is outstanding.
    let stored: Option<i64> =
        sqlx::query_scalar("select last_history_id from sync_state where account_id = $1")
            .bind(account)
            .fetch_optional(&db.pool)
            .await
            .unwrap()
            .flatten();
    assert_eq!(stored, Some(9_412_771));
}

/// **The defect the first live sync found, as a test.**
///
/// Gmail 429s a fraction of the messages. Every one of them must end up stored:
/// a rate limit is temporary and a skipped message is forever.
#[tokio::test]
async fn a_throttled_sync_stores_every_message() {
    let mut fixtures: Vec<Fixture> = (0..30).map(Fixture::new).collect();
    // A third of the window, throttled twice each, exactly as the live account
    // behaved: a `200` batch with `429` sub-responses inside it.
    for index in (0..30).step_by(3) {
        fixtures[index] = fixtures[index].clone().throttled_for(2);
    }

    let gmail = FakeGmail::start(fixtures).await;
    let (db, account) = connected().await;

    let report = run_sync(&db.pool, &gmail.client(), account, &fast_options())
        .await
        .expect("a transient throttle must not fail the sync");

    assert_eq!(report.listed, 30);
    assert_eq!(
        report.ingested, 30,
        "every throttled message must still land"
    );
    assert_eq!(report.unresolved, 0);
    assert_eq!(report.fetch_failures, 0, "a 429 is not a fetch failure");
    assert_eq!(
        report.retried, 10,
        "the ten throttled messages came back later"
    );
    assert!(report.retry_rounds >= 1, "the retry actually happened");

    let stored: i64 = sqlx::query_scalar("select count(*) from messages where account_id = $1")
        .bind(account)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(stored, 30, "no message may be missing after a throttle");

    // The throttle is on the record, and it is NOT recorded as a lost message.
    let throttled: i64 =
        sqlx::query_scalar("select count(*) from audit_log where action = 'gmail_sync_throttled'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(throttled >= 1);
    let lost: i64 =
        sqlx::query_scalar("select count(*) from audit_log where action = 'message_fetch_failed'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(lost, 0);

    // And the sync is allowed to say it finished.
    let completed: i64 =
        sqlx::query_scalar("select count(*) from audit_log where action = 'gmail_sync_completed'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(completed, 1);
}

/// **The other half of the same rule.** When the throttle does not lift, the
/// cursor must not move and the job must fail, so the queue fetches those
/// messages again. The live failure mode was the opposite: skip, then commit.
#[tokio::test]
async fn a_sync_that_cannot_resolve_everything_fails_and_leaves_the_cursor() {
    let mut fixtures: Vec<Fixture> = (0..5).map(Fixture::new).collect();
    // Throttled far more times than the sync will re-ask.
    fixtures[2] = fixtures[2].clone().throttled_for(1_000);

    let gmail = FakeGmail::start(fixtures).await;
    let (db, account) = connected().await;

    let error = run_sync(&db.pool, &gmail.client(), account, &fast_options())
        .await
        .expect_err("an unresolved message must fail the job, not complete it");
    let incomplete = error
        .downcast_ref::<SyncIncomplete>()
        .expect("the failure must be distinguishable from a database outage");
    assert_eq!(incomplete.unresolved, 1);
    assert_eq!(incomplete.listed, 5);
    assert_eq!(incomplete.rounds, MAX_RETRY_ROUNDS);

    // THE assertion: the cursor did not move, so the next sync re-lists and
    // re-fetches the message we could not get.
    let cursor: Option<i64> =
        sqlx::query_scalar("select last_history_id from sync_state where account_id = $1")
            .bind(account)
            .fetch_optional(&db.pool)
            .await
            .unwrap()
            .flatten();
    assert_eq!(
        cursor, None,
        "advancing history_id past an unfetched message deletes it from NADE forever"
    );

    // The four that did arrive are still stored - a failed sync is not a
    // rollback, it is an incomplete one.
    let stored: i64 = sqlx::query_scalar("select count(*) from messages where account_id = $1")
        .bind(account)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(stored, 4);

    // ...and their thread rollups exist, because rollups happen per batch. The
    // live sync ingested 176 messages, died before its single trailing rollup
    // loop, and left the mail list rendering nothing.
    let threads: i64 = sqlx::query_scalar("select count(*) from threads where account_id = $1")
        .bind(account)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(threads, 4, "partial progress must still be readable");

    // The audit trail says which of the two endings this was.
    let (incomplete_rows, completed_rows): (i64, i64) = sqlx::query_as(
        "select count(*) filter (where action = 'gmail_sync_incomplete'), \
                count(*) filter (where action = 'gmail_sync_completed') from audit_log",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(incomplete_rows, 1, "a sync that gave up says so");
    assert_eq!(completed_rows, 0, "and must never claim it completed");
}

/// A `5xx` is Gmail saying "not now", not "not ever", so it takes the same path
/// as a `429`: retried, and if it never lifts the sync fails rather than
/// silently dropping the message. This is the case that used to read
/// `report.fetch_failures == 1` and complete.
#[tokio::test]
async fn a_backend_error_is_transient_too() {
    let mut fixtures: Vec<Fixture> = (0..5).map(Fixture::new).collect();
    fixtures[1] = fixtures[1].clone().broken();

    let gmail = FakeGmail::start(fixtures).await;
    let (db, account) = connected().await;

    let error = run_sync(&db.pool, &gmail.client(), account, &fast_options())
        .await
        .expect_err("a 500 that never lifts must fail the sync");
    assert_eq!(
        error.downcast_ref::<SyncIncomplete>().map(|i| i.unresolved),
        Some(1)
    );

    let cursor: Option<i64> =
        sqlx::query_scalar("select last_history_id from sync_state where account_id = $1")
            .bind(account)
            .fetch_optional(&db.pool)
            .await
            .unwrap()
            .flatten();
    assert_eq!(cursor, None, "the cursor must not move over a 500");
}

/// Two workers, one account: the second finds the lock held and does nothing,
/// rather than doubling the number of requests Gmail counts for that user.
#[tokio::test]
async fn a_second_sync_of_the_same_account_stands_down() {
    let fixtures: Vec<Fixture> = (0..3).map(Fixture::new).collect();
    let gmail = FakeGmail::start(fixtures).await;
    let (db, account) = connected().await;

    // Hold the lock from outside, exactly as a first worker would.
    let mut held = db.pool.acquire().await.unwrap();
    let taken: bool = sqlx::query_scalar("select pg_try_advisory_lock($1, $2)")
        .bind(SYNC_LOCK_NAMESPACE)
        .bind(account_lock_key(account))
        .fetch_one(&mut *held)
        .await
        .unwrap();
    assert!(taken);

    let report = run_sync(&db.pool, &gmail.client(), account, &fast_options())
        .await
        .unwrap();
    assert!(report.skipped_concurrent, "the second sync must stand down");
    assert_eq!(report.listed, 0);
    assert!(
        gmail.server.received_requests().await.unwrap().is_empty(),
        "the second sync must not make a single Gmail call"
    );

    // Once the first worker is done, the next sync runs normally.
    sqlx::query("select pg_advisory_unlock($1, $2)")
        .bind(SYNC_LOCK_NAMESPACE)
        .bind(account_lock_key(account))
        .execute(&mut *held)
        .await
        .unwrap();
    drop(held);

    let report = run_sync(&db.pool, &gmail.client(), account, &fast_options())
        .await
        .unwrap();
    assert!(!report.skipped_concurrent);
    assert_eq!(report.ingested, 3);
}

/// Two live-probed Gmail behaviours, held here because neither is detectable at
/// runtime and both are easy to reintroduce.
///
/// 1. **A malformed `q` is not an error.** An unknown operator, an empty
///    `label:` and outright nonsense all return `200` with zero messages -
///    indistinguishable from an empty window. So the query must be a fixed
///    template over a validated integer and never string concatenation.
/// 2. **`includeSpamTrash=false` gates nothing.** `in:anywhere`, `in:trash` and
///    `in:spam` widen the scope past it regardless. Keeping Trash and Spam out
///    is a property of the query carrying no scope operator.
#[test]
fn the_sync_query_is_a_fixed_template_with_no_scope_operator() {
    for days in [1u32, 7, 30, 365] {
        let mut config = crate::config::tests::sample();
        config.gmail.sync_window_days = days;
        let query = SyncOptions::from_config(&config).query;

        assert_eq!(query, format!("newer_than:{days}d"));
        for operator in ["in:", "label:", "OR", "{", "}", "-"] {
            assert!(
                !query.contains(operator),
                "{operator:?} in the sync query would widen or narrow the scope silently: {query}"
            );
        }
    }

    // The template's only variable is an integer the config already refuses to
    // leave at zero, so there is no input that can make the query malformed.
    assert!(
        crate::config::tests::sample().gmail.sync_window_days >= 1,
        "a zero-day window would produce `newer_than:0d`, which Gmail answers \
         with an empty page rather than an error"
    );
}

/// The retry schedule, asserted without sleeping through it.
#[test]
fn retry_delay_is_exponential_and_capped() {
    let options = SyncOptions {
        query: String::new(),
        max_messages: 1,
        batch_size: 1,
        batch_interval: std::time::Duration::ZERO,
        max_retry_rounds: MAX_RETRY_ROUNDS,
        retry_backoff: std::time::Duration::from_secs(2),
    };
    let schedule: Vec<u64> = (0..8)
        .map(|round| options.retry_delay(round).as_secs())
        .collect();
    assert_eq!(schedule, vec![2, 4, 8, 16, 32, 60, 60, 60]);
    assert!(
        options
            .retry_delay(MAX_RETRY_ROUNDS)
            .le(&crate::gmail::quota::MAX_BACKOFF),
        "no retry may outlast the job lease"
    );
}

/// Criteria N6 + N7 - mandated edge cases 3 (crash mid-step) and 4 (duplicate
/// delivery / replay).
#[tokio::test]
async fn a_replayed_sync_is_idempotent() {
    let fixtures: Vec<Fixture> = (0..6).map(Fixture::new).collect();
    let gmail = FakeGmail::start(fixtures).await;
    let (db, account) = connected().await;

    let first = run_sync(&db.pool, &gmail.client(), account, &fast_options())
        .await
        .unwrap();
    // A crash after the first sync looks exactly like this: the job is claimed
    // again and the whole thing re-runs.
    let second = run_sync(&db.pool, &gmail.client(), account, &fast_options())
        .await
        .unwrap();

    assert_eq!(first.ingested, second.ingested);

    let (messages, threads, labels, attachments): (i64, i64, i64, i64) = sqlx::query_as(
        "select (select count(*) from messages where account_id = $1), \
                (select count(*) from threads where account_id = $1), \
                (select count(*) from thread_labels where account_id = $1), \
                (select count(*) from attachments)",
    )
    .bind(account)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(messages, 6, "a replay must not duplicate the mailbox");
    assert_eq!(threads, 6);
    assert_eq!(labels, 12, "two labels per thread, not four");
    assert_eq!(attachments, 0);
}

/// Criterion N8.
#[tokio::test]
async fn labels_are_stored_including_the_ones_the_api_hides() {
    let gmail = FakeGmail::start(vec![Fixture::new(0)]).await;
    let (db, account) = connected().await;

    let report = run_sync(&db.pool, &gmail.client(), account, &fast_options())
        .await
        .unwrap();
    assert_eq!(report.labels, 6);

    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "select label_id, name, type from labels where account_id = $1 order by label_id",
    )
    .bind(account)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 6);
    assert!(rows
        .iter()
        .any(|(id, name, kind)| id == "Label_12" && name == "To Reply" && kind == "user"));
    // Stored, not dropped: the API hides `[Gmail]` prefixes, so a later phase
    // can change its mind without a re-sync.
    assert!(rows.iter().any(|(_, name, _)| name == "[Gmail]All Mail"));
}

/// Criterion N9.
#[tokio::test]
async fn thread_rollups_follow_the_newest_message() {
    let fixtures = vec![
        Fixture::new(0)
            .with_thread("thread-a")
            .at(1_786_950_000_000)
            .with_raw(rfc822("Old Sender <old@example.com>", "First", "Oldest.")),
        Fixture::new(1)
            .with_thread("thread-a")
            .at(1_786_960_000_000)
            .with_labels(&["INBOX", "UNREAD"])
            .with_raw(rfc822(
                "Priya Raghavan <priya@kettle.com>",
                "Re: First",
                "Newest message wins.",
            )),
        Fixture::new(2)
            .with_thread("thread-b")
            .at(1_786_940_000_000)
            .with_labels(&["SENT"])
            .with_raw(rfc822("Me <me@example.com>", "Sent thing", "Sent body.")),
    ];

    let gmail = FakeGmail::start(fixtures).await;
    let (db, account) = connected().await;
    run_sync(&db.pool, &gmail.client(), account, &fast_options())
        .await
        .unwrap();

    let (subject, snippet, from_name, from_email, unread, msg_count, last_ts): (
        String,
        String,
        String,
        String,
        bool,
        i32,
        DateTime<Utc>,
    ) = sqlx::query_as(
        "select subject, snippet, from_name, from_email, unread, msg_count, last_ts \
           from threads where account_id = $1 and thread_id = 'thread-a'",
    )
    .bind(account)
    .fetch_one(&db.pool)
    .await
    .unwrap();

    assert_eq!(subject, "Re: First", "the newest message names the thread");
    assert_eq!(from_name, "Priya Raghavan");
    assert_eq!(from_email, "priya@kettle.com");
    assert_eq!(snippet, "Newest message wins.");
    assert!(unread, "ANY unread message makes the thread unread");
    assert_eq!(msg_count, 2);
    assert_eq!(last_ts.timestamp_millis(), 1_786_960_000_000);

    // The thread is filed under the union of its messages' labels.
    let mut labels: Vec<String> = sqlx::query_scalar(
        "select label_id from thread_labels \
          where account_id = $1 and thread_id = 'thread-a' order by label_id",
    )
    .bind(account)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    labels.sort();
    assert_eq!(labels, vec!["CATEGORY_PERSONAL", "INBOX", "UNREAD"]);

    // A thread with no unread message is not unread.
    let unread_b: bool = sqlx::query_scalar(
        "select unread from threads where account_id = $1 and thread_id = 'thread-b'",
    )
    .bind(account)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(!unread_b);
}

/// Criterion N11.
#[tokio::test]
async fn sync_is_paused_when_the_account_needs_reauth() {
    let gmail = FakeGmail::start((0..3).map(Fixture::new).collect()).await;
    let (db, account) = connected().await;
    sqlx::query("update accounts set status = 'needs_reauth' where id = $1")
        .bind(account)
        .execute(&db.pool)
        .await
        .unwrap();

    let report = run_sync(&db.pool, &gmail.client(), account, &fast_options())
        .await
        .expect("a paused sync succeeds; failing would just burn the retry budget");

    assert!(report.paused);
    assert_eq!(report.ingested, 0);
    assert!(
        gmail.server.received_requests().await.unwrap().is_empty(),
        "a paused account must not touch Gmail at all"
    );
}

/// Criterion N13 - mandated edge case 1 (empty input).
#[tokio::test]
async fn an_empty_window_is_not_an_error() {
    let gmail = FakeGmail::start(Vec::new()).await;
    let (db, account) = connected().await;

    let report = run_sync(&db.pool, &gmail.client(), account, &fast_options())
        .await
        .unwrap();
    assert_eq!(report.listed, 0);
    assert_eq!(report.ingested, 0);
    assert_eq!(report.batches, 0, "no messages means no batch request");
    assert_eq!(report.history_id, Some(9_412_771));

    let batches = gmail
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path().ends_with("/batch/gmail/v1"))
        .count();
    assert_eq!(batches, 0);

    // The cursor is still recorded, so the first incremental sync has somewhere
    // to start from.
    let stored: Option<i64> =
        sqlx::query_scalar("select last_history_id from sync_state where account_id = $1")
            .bind(account)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(stored, Some(9_412_771));
}

/// Criterion N14 - mandated edge case 2 (unicode).
#[tokio::test]
async fn unicode_survives_ingest() {
    let fixtures = vec![Fixture::new(0).with_raw(rfc822(
        "=?utf-8?B?RsO2aG4gVmVyc2FuZA==?= <versand@foehn.de>",
        "=?utf-8?B?SWhyZSBCZXN0ZWxsdW5nIGlzdCB1bnRlcndlZ3Mg8J+Tpg==?=",
        "Ihre Bestellung Nr. 88-2041 東京で会いましょう 🚀",
    ))];
    let gmail = FakeGmail::start(fixtures).await;
    let (db, account) = connected().await;
    run_sync(&db.pool, &gmail.client(), account, &fast_options())
        .await
        .unwrap();

    let (subject, from_name, body): (String, String, String) =
        sqlx::query_as("select subject, from_name, body_text from messages where account_id = $1")
            .bind(account)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(subject, "Ihre Bestellung ist unterwegs 📦");
    assert_eq!(from_name, "Föhn Versand");
    assert!(body.contains("東京で会いましょう 🚀"), "{body}");

    // And it is searchable through the generated tsvector.
    let hits: i64 = sqlx::query_scalar(
        "select count(*) from messages \
          where account_id = $1 and fts @@ websearch_to_tsquery('simple', 'Bestellung')",
    )
    .bind(account)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(hits, 1);
}

/// Criterion N15 - mandated edge case 8 (clock skew).
#[tokio::test]
async fn internal_date_wins_over_the_date_header() {
    // The sender's clock says 2026-08-16T09:12:04Z; Gmail received it at
    // 2026-08-17T00:00:00Z. Gmail's clock is the one we trust.
    let received = 1_787_011_200_000i64;
    let fixtures = vec![Fixture::new(0).at(received)];
    let gmail = FakeGmail::start(fixtures).await;
    let (db, account) = connected().await;
    run_sync(&db.pool, &gmail.client(), account, &fast_options())
        .await
        .unwrap();

    let ts: DateTime<Utc> =
        sqlx::query_scalar("select internal_ts from messages where account_id = $1")
            .bind(account)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(ts.timestamp_millis(), received);

    // ...but a message Gmail gave no internalDate for still gets the header's
    // time rather than nothing.
    let parsed = crate::mail::parse::parse(&rfc822("a@b.com", "S", "B"), "g").unwrap();
    let bare = crate::gmail::types::GmailMessage {
        id: "g".to_owned(),
        ..Default::default()
    };
    let row = store::IngestRow::parsed(&bare, parsed);
    assert_eq!(
        row.internal_ts.unwrap().to_rfc3339(),
        "2026-08-16T09:12:04+00:00"
    );
}

/// Criterion N12.
#[tokio::test]
async fn gmail_sync_is_a_registered_job_kind() {
    let app = crate::test_support::test_app(crate::config::Env::Dev).await;
    let mut registry = crate::jobs::Registry::new();
    crate::register_handlers(&mut registry, &app.state);

    assert!(
        registry.kinds().contains(&KIND.to_owned()),
        "gmail_sync must be claimable: {:?}",
        registry.kinds()
    );

    // And a queued job with no account is a clean no-op rather than a failure
    // that burns the retry budget.
    let queue = crate::jobs::Queue::new(app.db.pool.clone(), crate::jobs::QueueConfig::default());
    let id = queue
        .enqueue(KIND, &serde_json::json!({}), None)
        .await
        .unwrap();
    let job = queue
        .claim("w1", &[KIND.to_owned()])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.id, id);

    let handler = registry.get(KIND).unwrap();
    handler
        .handle(job, crate::jobs::JobContext { queue })
        .await
        .expect("no account connected is not an error");
}

/// Criterion N10 - **ingest never calls an LLM.** Enforced by grep, exactly like
/// the ban on compile-time sqlx macros, because a comment would not have stopped
/// the prior art either.
#[test]
fn ingest_never_calls_an_llm() {
    let roots = [
        std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src/sync")),
        std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src/mail")),
    ];
    // Assembled at runtime so this test file is not its own offender. Each
    // needle is a *provider*, an *SDK*, or a *model id* - never a NADE route,
    // because `/v1/messages/{id}/attachments/{att}` is our own attachment proxy.
    let banned: Vec<String> = [
        ("anthro", "pic"),
        ("open", "ai"),
        ("nade_agent", "_sdk"),
        ("::L", "lm"),
        ("ll", "m::"),
        ("chat_comp", "letion"),
        ("gpt", "-4"),
        ("claude", "-3"),
        ("api.", "cohere"),
        ("mist", "ral"),
    ]
    .iter()
    .map(|(head, tail)| format!("{head}{tail}"))
    .collect();

    let mut offenders = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = roots.into();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for (number, line) in text.lines().enumerate() {
                let trimmed = line.trim_start();
                // The doc comments here talk *about* not calling an LLM.
                if trimmed.starts_with("//") || trimmed.starts_with("*") {
                    continue;
                }
                let lowered = line.to_lowercase();
                if banned
                    .iter()
                    .any(|needle| lowered.contains(needle.as_str()))
                {
                    offenders.push(format!("{}:{}: {}", path.display(), number + 1, trimmed));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "ingest must never call an LLM (PLAN.md §Gmail sync 3):\n{}",
        offenders.join("\n")
    );
}
