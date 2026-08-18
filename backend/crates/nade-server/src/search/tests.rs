//! Search, end to end against `nade-gmail-sim`.
//!
//! A stateful fake with a **real query parser**, not stubs: the behaviour under
//! test *is* Gmail's query language - an unknown operator answered with an
//! empty `200`, `label:` taking a name and never an id, `newer_than:1w`
//! matching nothing. A hand-written stub would only echo back whatever we
//! already believed, and what we already believed is exactly what
//! `docs/SEARCH.md` was written to correct.
//!
//! One test uses `wiremock` instead: the ordering test needs a `messages.list`
//! whose order is deliberately *not* chronological, and the simulator - like
//! any honest fake - returns newest first.

use std::sync::Arc;

use axum::http::StatusCode;
use nade_gmail_sim::{
    http::SimServer,
    message::{MessageSpec, ReceivedAt},
    Simulator,
};
use serde_json::Value;
use uuid::Uuid;

use super::*;
use crate::{
    config::Env,
    test_support::{get as authed_get, response_json, test_app, TestApp},
};

/// One day, in milliseconds.
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

struct Harness {
    app: TestApp,
    token: String,
    account: Uuid,
    server: SimServer,
}

impl Harness {
    fn sim(&self) -> &Arc<Simulator> {
        self.server.simulator()
    }

    fn now_ms(&self) -> i64 {
        self.sim().clock().now_ms()
    }

    /// Seed a message `days` days before the simulator's "now".
    fn seed(&self, days: i64, subject: &str, body: &str) -> String {
        self.sim()
            .insert_message(
                &MessageSpec::new()
                    .subject(subject)
                    .text(body)
                    .received_at(ReceivedAt::AtMs(self.now_ms() - days * DAY_MS)),
            )
            .expect("seeding the fake mailbox")
    }

    async fn search(&self, query: &str) -> Value {
        let (status, body) = self.search_status(query).await;
        assert_eq!(status, StatusCode::OK, "GET /v1/search?q={query}: {body}");
        body
    }

    async fn search_status(&self, query: &str) -> (StatusCode, Value) {
        let path = format!("/v1/search?q={}", encode(query));
        let response = authed_get(&self.app, &path, Some(&self.token)).await;
        let status = response.status();
        (status, response_json(response).await)
    }

    /// Run the real 30-day sync, so "inside the window" means what it means in
    /// production rather than what a test decided it meant.
    async fn sync_thirty_days(&self) -> crate::sync::SyncReport {
        let client = self.app.state.gmail.client_for(self.account);
        crate::sync::run_sync(
            &self.app.db.pool,
            &client,
            self.account,
            &crate::sync::SyncOptions {
                query: "newer_than:30d".to_owned(),
                max_messages: 500,
                batch_size: crate::gmail::client::MAX_BATCH,
                batch_interval: std::time::Duration::ZERO,
                max_retry_rounds: 1,
                retry_backoff: std::time::Duration::ZERO,
            },
        )
        .await
        .expect("the 30-day sync")
    }

    async fn cached_gmail_ids(&self) -> Vec<String> {
        sqlx::query_scalar("select gmail_id from messages where account_id = $1 order by gmail_id")
            .bind(self.account)
            .fetch_all(&self.app.db.pool)
            .await
            .unwrap()
    }
}

fn encode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// A paired device, a connected account, and a fake Gmail the whole stack -
/// REST, batch and OAuth - is pointed at.
async fn harness() -> Harness {
    let server = SimServer::start(Arc::new(Simulator::new()))
        .await
        .expect("binding the fake Gmail");

    let mut app = test_app(Env::Prod).await;
    app.set_gmail_base(&server.base_url());
    let token = app.device_token().await;

    let account: Uuid = sqlx::query_scalar(
        "insert into accounts (email, status) values ('jatinsethi98@gmail.com', 'ok') returning id",
    )
    .fetch_one(&app.db.pool)
    .await
    .unwrap();

    // The simulator enforces its bearer token, so the stored credential has to
    // be the one it will accept.
    let cipher = app.gmail_cipher();
    sqlx::query(
        "insert into gmail_tokens (account_id, access_token, access_expiry, refresh_token, scopes) \
         values ($1, $2, now() + interval '1 hour', $3, array['gmail.readonly'])",
    )
    .bind(account)
    .bind(cipher.encrypt(&server.simulator().access_token()).unwrap())
    .bind(cipher.encrypt(&server.simulator().refresh_token()).unwrap())
    .execute(&app.db.pool)
    .await
    .unwrap();

    Harness {
        app,
        token,
        account,
        server,
    }
}

fn thread_ids(body: &Value) -> Vec<String> {
    body["threads"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a threads array, got {body}"))
        .iter()
        .map(|thread| thread["id"].as_str().unwrap().to_owned())
        .collect()
}

fn subjects(body: &Value) -> Vec<String> {
    body["threads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|thread| thread["subject"].as_str().unwrap().to_owned())
        .collect()
}

// ------------------------------------------------------- the whole point --

/// **The test that could not have passed before.**
///
/// The `fts` column indexed the 30-day sync window: ~500 of 63,866 messages.
/// A search for an invoice from March returned zero rows, indistinguishable
/// from "no such mail exists". Here the message is 400 days old, the real sync
/// genuinely does not ingest it, and the search finds it anyway - because the
/// index is Gmail's, not ours.
#[tokio::test]
async fn a_search_finds_a_message_outside_the_sync_window() {
    let harness = harness().await;
    harness.seed(2, "Recent standup notes", "nothing to report");
    let old = harness.seed(
        400,
        "Rechnung 88-2041 für Café München",
        "Ihre Bestellung ist unterwegs 📦",
    );

    // The window really is a window: the 30-day sync does not have this
    // message, so nothing local could ever have matched it.
    let report = harness.sync_thirty_days().await;
    assert_eq!(
        report.listed, 1,
        "only the recent message is inside 30 days"
    );
    let cached = harness.cached_gmail_ids().await;
    assert!(
        !cached.contains(&old),
        "the 400-day-old message must be outside the synced window, or this test proves nothing"
    );

    // And the search finds it regardless.
    let body = harness.search("Rechnung").await;
    assert_eq!(
        subjects(&body),
        vec!["Rechnung 88-2041 für Café München".to_owned()],
        "Gmail indexes the whole mailbox; the 30-day cache is not the index"
    );
    assert!(body["next_cursor"].is_null());

    // Unicode reaches Gmail intact through the query string.
    let body = harness.search("München").await;
    assert_eq!(thread_ids(&body).len(), 1);
}

/// The cache warms as a side effect: a message found by search is a message
/// opening the thread never has to fetch again.
#[tokio::test]
async fn a_search_hydrates_a_cache_miss_and_leaves_it_cached() {
    let harness = harness().await;
    let old = harness.seed(365, "Invoice 4471 from Halcyon", "The attached invoice…");

    assert!(
        harness.cached_gmail_ids().await.is_empty(),
        "nothing is cached before the search"
    );

    let body = harness.search("Halcyon").await;
    assert_eq!(
        subjects(&body),
        vec!["Invoice 4471 from Halcyon".to_owned()]
    );

    // Stored, in full, by the same ingest path the sync uses.
    assert_eq!(harness.cached_gmail_ids().await, vec![old.clone()]);
    let (subject, body_text, thread_id): (String, String, String) = sqlx::query_as(
        "select subject, body_text, thread_id from messages \
          where account_id = $1 and gmail_id = $2",
    )
    .bind(harness.account)
    .bind(&old)
    .fetch_one(&harness.app.db.pool)
    .await
    .unwrap();
    assert_eq!(subject, "Invoice 4471 from Halcyon");
    assert!(body_text.contains("The attached invoice"), "{body_text}");

    // The thread rollup exists too, so `GET /threads/{id}` serves it locally.
    let detail = response_json(
        authed_get(
            &harness.app,
            &format!("/v1/threads/{thread_id}"),
            Some(&harness.token),
        )
        .await,
    )
    .await;
    assert_eq!(detail["messages"][0]["gmail_id"], old);

    // A second search for the same message spends no `messages.get` at all:
    // the miss became a hit.
    harness.sim().clear_calls();
    let body = harness.search("Halcyon").await;
    assert_eq!(
        subjects(&body),
        vec!["Invoice 4471 from Halcyon".to_owned()]
    );
    let gets = harness
        .sim()
        .calls()
        .into_iter()
        .filter(|call| call.in_batch)
        .count();
    assert_eq!(
        gets, 0,
        "a warm page costs one messages.list and nothing else"
    );
}

/// What a page actually costs, measured rather than asserted in prose.
///
/// This is the price of delegating search, and it is the number to watch: a
/// **cold** page of 50 misses is one `messages.list` plus `ceil(50/10)` batches
/// - 6 sequential round trips and 5 + 50x5 = 255 quota units against a
/// 250-units-per-second ceiling, so it also pays about a second of the client's
/// own pacing. A **warm** page is one round trip and 5 units.
#[tokio::test]
async fn a_cold_page_costs_one_list_plus_one_batch_per_ten_misses() {
    let harness = harness().await;
    for index in 0..25 {
        harness.seed(
            200 + i64::from(index),
            &format!("Quota probe {index:02}"),
            "an entry",
        );
    }

    harness.sim().clear_calls();
    let body = harness.search("probe").await;
    assert_eq!(thread_ids(&body).len(), 25);

    let calls = harness.sim().calls();
    let lists = calls
        .iter()
        .filter(|call| !call.in_batch && call.target.contains("/messages?"))
        .count();
    let batches = calls
        .iter()
        .filter(|call| !call.in_batch && call.target.contains("/batch/"))
        .count();
    let gets = calls.iter().filter(|call| call.in_batch).count();

    assert_eq!(
        lists, 1,
        "one messages.list per page, whatever the hit rate"
    );
    assert_eq!(
        gets, 25,
        "one messages.get per miss, and not one per result"
    );
    assert_eq!(
        batches,
        25_usize.div_ceil(crate::gmail::client::MAX_BATCH),
        "misses are batched {} at a time",
        crate::gmail::client::MAX_BATCH
    );

    // 5 units for the list, 5 per get.
    let units = 5 + 5 * gets;
    assert_eq!(units, 130);
}

// ------------------------------------------------------------- the query --

/// Every rejection the validator makes is a 400 with its explanation on the
/// wire, and - the half that matters - the simulator confirms Gmail would have
/// answered each of them with a silent empty `200`.
#[tokio::test]
async fn a_refused_query_is_a_400_that_says_why_and_gmail_would_have_said_nothing() {
    let harness = harness().await;
    harness.seed(1, "Subscriptions digest", "weekly roundup");
    harness.seed(1, "Another message", "hello");

    for (query, fragments) in [
        (
            "labels:Subscriptions",
            vec!["Unknown operator", "`labels:`"],
        ),
        ("newer_than:1w", vec!["Weeks are not a unit", "7d"]),
        ("newer_than:7", vec!["bare number"]),
        ("after:yesterday", vec!["YYYY/MM/DD"]),
        ("is:unseen", vec!["`is:unseen`", "`unread`"]),
        ("from:", vec!["no argument", "match-everything"]),
        ("invoice or receipt", vec!["boolean OR"]),
    ] {
        let (status, body) = harness.search_status(query).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{query:?} must be refused, not passed through to a silent empty page"
        );
        assert_eq!(body["error"]["code"], "bad_request", "{query:?}");
        let message = body["error"]["message"].as_str().unwrap();
        for fragment in fragments {
            assert!(
                message.contains(fragment),
                "{query:?}: the 400 must explain itself; wanted {fragment:?}, got {message}"
            );
        }

        // The other half of the claim: Gmail really would have swallowed it.
        // Everything except `from:`, which matches *everything* rather than
        // nothing - the one that is dangerous in the other direction.
        let raw = raw_list(&harness, query).await;
        if query == "from:" {
            assert_eq!(
                raw, 2,
                "an empty argument is a match-all no-op, which is why it is refused"
            );
        } else {
            assert_eq!(
                raw, 0,
                "{query:?} matches nothing at Gmail and returns 200, so only we can report it"
            );
        }
    }
}

/// Ask the fake Gmail directly, bypassing our validator, and count the rows.
async fn raw_list(harness: &Harness, query: &str) -> usize {
    let sim = harness.sim();
    let response = sim.handle(&sim.authorized(&format!(
        "/gmail/v1/users/me/messages?maxResults=100&q={}",
        encode(query)
    )));
    assert_eq!(
        response.status, 200,
        "Gmail never answers a bad `q` with anything but 200 - that is the whole problem"
    );
    response.json_body()["messages"]
        .as_array()
        .map_or(0, Vec::len)
}

/// `label:` takes the **name**. Handed the id - which is exactly what a program
/// has to hand - Gmail returns zero. We translate instead.
#[tokio::test]
async fn a_label_id_is_translated_rather_than_silently_matching_nothing() {
    let harness = harness().await;

    let label = harness
        .sim()
        .mailbox_mut(|mailbox| mailbox.create_label("Subscriptions"))
        .expect("creating the label");
    harness
        .sim()
        .insert_message(
            &MessageSpec::new()
                .subject("Your weekly digest")
                .text("five things")
                .labels(["INBOX".to_owned(), label.clone()])
                .received_at(ReceivedAt::AtMs(harness.now_ms() - 200 * DAY_MS)),
        )
        .unwrap();

    // As the sync would have stored them.
    for (id, name, kind) in [
        ("INBOX", "INBOX", "system"),
        (label.as_str(), "Subscriptions", "user"),
    ] {
        sqlx::query("insert into labels (account_id, label_id, name, type) values ($1,$2,$3,$4)")
            .bind(harness.account)
            .bind(id)
            .bind(name)
            .bind(kind)
            .execute(&harness.app.db.pool)
            .await
            .unwrap();
    }

    // The trap, measured against the fake: the id matches nothing.
    assert_eq!(raw_list(&harness, &format!("label:{label}")).await, 0);
    assert_eq!(raw_list(&harness, "label:Subscriptions").await, 1);

    // And the endpoint takes the id anyway, because it translates it.
    let body = harness.search(&format!("label:{label}")).await;
    assert_eq!(subjects(&body), vec!["Your weekly digest".to_owned()]);
    let body = harness.search("label:Subscriptions").await;
    assert_eq!(subjects(&body), vec!["Your weekly digest".to_owned()]);

    // An id we have never seen cannot be translated, so it is refused with the
    // name to use rather than passed through to zero results.
    let (status, body) = harness.search_status("label:Label_404").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("Subscriptions"), "{message}");
}

/// `includeSpamTrash=false` gates neither `q` nor `labelIds`. A caller that
/// writes `in:anywhere` means it, and gets it.
#[tokio::test]
async fn a_scope_operator_reaches_past_trash_and_spam() {
    let harness = harness().await;
    let kept = harness.seed(3, "Kettle interview loop", "two slots");
    let trashed = harness.seed(3, "Kettle rejected posting", "no longer available");
    harness.sim().trash_message(&trashed).unwrap();

    let body = harness.search("Kettle").await;
    assert_eq!(
        thread_ids(&body).len(),
        1,
        "the default scope leaves Trash out"
    );

    let body = harness.search("Kettle in:anywhere").await;
    let ids = thread_ids(&body);
    assert_eq!(ids.len(), 2, "in:anywhere widens past Trash: {ids:?}");
    assert!(ids.contains(&kept));
    assert!(ids.contains(&trashed));
}

// --------------------------------------------------------------- ordering --

/// **Gmail's relevance order is the product.** Re-sorting by date here would
/// throw away the ranking that is the whole reason for delegating search.
///
/// `wiremock` rather than the simulator: this needs a `messages.list` whose
/// order is deliberately not chronological, and an honest fake returns newest
/// first.
#[tokio::test]
async fn results_keep_gmails_order_and_are_not_re_sorted_by_date() {
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    let server = MockServer::start().await;
    let mut app = test_app(Env::Prod).await;
    app.set_gmail_base(&server.uri());
    let token = app.device_token().await;
    let account: Uuid = sqlx::query_scalar(
        "insert into accounts (email, status) values ('jatinsethi98@gmail.com', 'ok') returning id",
    )
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    let cipher = app.gmail_cipher();
    sqlx::query(
        "insert into gmail_tokens (account_id, access_token, access_expiry, refresh_token, scopes) \
         values ($1, $2, now() + interval '1 hour', $3, array['gmail.readonly'])",
    )
    .bind(account)
    .bind(cipher.encrypt("ya29.ranked").unwrap())
    .bind(cipher.encrypt("1//refresh").unwrap())
    .execute(&app.db.pool)
    .await
    .unwrap();

    // Three threads, already cached, whose date order is the exact reverse of
    // the order Gmail is about to rank them in.
    for (index, gmail_id) in ["oldest", "middle", "newest"].into_iter().enumerate() {
        let parsed = crate::mail::parse::ParsedMessage {
            subject: gmail_id.to_owned(),
            from_email: "priya@kettle.com".to_owned(),
            body_text: "body".to_owned(),
            ..Default::default()
        };
        let message = crate::gmail::types::GmailMessage {
            id: gmail_id.to_owned(),
            thread_id: Some(format!("t-{gmail_id}")),
            label_ids: vec!["INBOX".to_owned()],
            internal_date: Some((1_786_950_724_000i64 + index as i64 * 86_400_000).to_string()),
            ..Default::default()
        };
        let row = crate::sync::store::IngestRow::parsed(&message, parsed);
        let mut tx = app.db.pool.begin().await.unwrap();
        crate::sync::store::upsert_message(&mut tx, account, &row)
            .await
            .unwrap();
        crate::sync::store::refresh_thread(&mut tx, account, &row.thread_id)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    // Gmail's ranking: oldest first, which no date sort would ever produce.
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                br#"{"messages":[{"id":"oldest","threadId":"t-oldest"},
                             {"id":"newest","threadId":"t-newest"},
                             {"id":"middle","threadId":"t-middle"}],
                 "resultSizeEstimate":3}"#
                    .to_vec(),
                "application/json",
            ),
        )
        .mount(&server)
        .await;

    let body = response_json(authed_get(&app, "/v1/search?q=body", Some(&token)).await).await;
    assert_eq!(
        thread_ids(&body),
        vec![
            "t-oldest".to_owned(),
            "t-newest".to_owned(),
            "t-middle".to_owned()
        ],
        "the page must be in Gmail's order; re-sorting by date is what the old \
         `order by last_ts desc` did and it discards the ranking"
    );

    // Nothing was fetched: every id was already cached.
    let batches = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path().ends_with("/batch/gmail/v1"))
        .count();
    assert_eq!(batches, 0);
}

// -------------------------------------------------------------- paging --

/// Pagination is Gmail's `pageToken`, wrapped in one of our opaque cursors.
#[tokio::test]
async fn pages_walk_gmails_page_token_through_an_opaque_cursor() {
    let harness = harness().await;
    // Comfortably more than one page of 50.
    for index in 0..60 {
        harness.seed(
            100 + i64::from(index),
            &format!("Ledger entry {index:02}"),
            "an entry",
        );
    }

    let first = harness.search("Ledger").await;
    let first_ids = thread_ids(&first);
    assert_eq!(first_ids.len(), PAGE_SIZE);
    let cursor = first["next_cursor"]
        .as_str()
        .expect("a second page")
        .to_owned();

    // Opaque, and a `pageToken` is nowhere to be seen in it.
    assert!(
        cursor
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "{cursor} must survive a query string untouched"
    );

    let second = response_json(
        authed_get(
            &harness.app,
            &format!("/v1/search?q=Ledger&cursor={cursor}"),
            Some(&harness.token),
        )
        .await,
    )
    .await;
    let second_ids = thread_ids(&second);
    assert_eq!(second_ids.len(), 10);
    assert!(
        second["next_cursor"].is_null(),
        "the last page has no cursor"
    );

    let all: std::collections::BTreeSet<&String> =
        first_ids.iter().chain(second_ids.iter()).collect();
    assert_eq!(all.len(), 60, "no duplicate, no skip across the boundary");
}

/// Criterion O6, for the search cursor: every way of being wrong is a 400,
/// never a silent reset to page one - **including** a perfectly valid keyset
/// cursor, which is a position in a different kind of list.
#[tokio::test]
async fn a_corrupt_search_cursor_is_a_bad_request() {
    let harness = harness().await;
    harness.seed(1, "anything", "at all");

    let keyset = crate::api::cursor::encode(chrono::Utc::now(), "t0");
    for bad in [
        "garbage",
        "%20",
        "eyJvZmZzZXQiOjUwfQ",
        // Valid base64url, valid JSON, wrong shape.
        "eyJwdCI6IiJ9",
        keyset.as_str(),
    ] {
        let response = authed_get(
            &harness.app,
            &format!("/v1/search?q=anything&cursor={bad}"),
            Some(&harness.token),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "cursor {bad:?} must be refused"
        );
        assert_eq!(
            response_json(response).await["error"]["code"],
            "bad_request"
        );
    }
}

// ---------------------------------------------------------------- shape --

/// The wire shape is unchanged: `docs/API.md` §2's threads list, plus
/// `next_cursor`. Only the implementation moved.
#[tokio::test]
async fn a_search_row_matches_the_thread_list_contract() {
    let harness = harness().await;
    harness.seed(90, "Staff Product Designer at Kettle", "own the composer");

    let body = harness.search("composer").await;
    let mut keys: Vec<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["next_cursor", "threads"]);

    let thread = &body["threads"][0];
    let mut keys: Vec<&str> = thread
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "agent_note",
            "from_email",
            "from_name",
            "id",
            "msg_count",
            "snippet",
            "subject",
            "ts",
            "unread"
        ]
    );
    assert_eq!(thread["subject"], "Staff Product Designer at Kettle");
    assert!(thread["agent_note"].is_null(), "no agent runs until P4");
    assert!(
        !thread["ts"].as_str().unwrap().contains('.'),
        "second precision, never milliseconds"
    );

    // EDGE (empty input at the far end): no hits is an empty array plus
    // `next_cursor: null`, never a 404.
    let body = harness.search("zzzznothingatall").await;
    assert_eq!(body["threads"].as_array().unwrap().len(), 0);
    assert!(body["next_cursor"].is_null());
}

/// Before any Gmail account is connected there is nothing to search - but a
/// malformed query is still a 400 rather than an empty page, because "we found
/// nothing" and "you asked wrong" must never look alike.
#[tokio::test]
async fn with_no_account_connected_the_query_is_still_validated() {
    let app = test_app(Env::Prod).await;
    let token = app.device_token().await;

    let body = response_json(authed_get(&app, "/v1/search?q=invoice", Some(&token)).await).await;
    assert_eq!(body["threads"].as_array().unwrap().len(), 0);
    assert!(body["next_cursor"].is_null());

    for (query, status) in [
        ("", StatusCode::BAD_REQUEST),
        ("%20%20", StatusCode::BAD_REQUEST),
        ("newer_than%3A1w", StatusCode::BAD_REQUEST),
        ("wibble%3Awobble", StatusCode::BAD_REQUEST),
    ] {
        let response = authed_get(&app, &format!("/v1/search?q={query}"), Some(&token)).await;
        assert_eq!(response.status(), status, "{query:?}");
    }
}

// ----------------------------------------------------------- the seam --

/// The seam P4's `search_mail` tool wires into.
///
/// `crate::search::search` is the *whole* search: it takes the raw query a
/// model wrote, validates it with the explanation the model needs to correct
/// itself, and returns the same rows the endpoint serves. A tool built on this
/// cannot drift from `GET /v1/search`, because there is nothing to drift from -
/// the endpoint is a five-line wrapper over this call.
#[tokio::test]
async fn the_search_seam_is_callable_without_the_http_layer() {
    let harness = harness().await;
    harness.seed(300, "Kettle offer letter", "attached, please sign");

    // What a tool call looks like: no router, no bearer token, no `Json`.
    let page = super::search(&harness.app.state, "Kettle", None)
        .await
        .expect("the tool path is the endpoint path");
    assert_eq!(page.threads.len(), 1);
    assert_eq!(page.threads[0].subject, "Kettle offer letter");
    assert!(page.next_cursor.is_none());

    // And a query the model got wrong comes back as a sentence it can act on,
    // not as an empty list it cannot distinguish from an empty mailbox.
    let error = super::search(&harness.app.state, "label:Label_25", None)
        .await
        .expect_err("a label id must not silently match nothing");
    assert_eq!(error.code, crate::error::ErrorCode::BadRequest);
    assert!(
        error.message.contains("label id") && error.message.contains("name"),
        "{}",
        error.message
    );

    // The endpoint serialises exactly this, field for field.
    let body = harness.search("Kettle").await;
    assert_eq!(subjects(&body), vec!["Kettle offer letter".to_owned()]);
}

/// Gmail failing is a `502`, not a partial page dressed up as a result.
/// `docs/SEARCH.md`: offline search degrades, and says so.
#[tokio::test]
async fn an_upstream_failure_is_a_502_rather_than_a_partial_page() {
    use nade_gmail_sim::fault::{Condition, Fault, FaultRule};

    let harness = harness().await;
    harness.seed(5, "Kettle interview", "two slots");

    harness.sim().inject(
        FaultRule::new(Fault::Unavailable {
            retry_after_seconds: None,
        })
        .when(Condition::PathContains("/messages".to_owned())),
    );

    let (status, body) = harness.search_status("Kettle").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["code"], "upstream_unavailable");
}

// ------------------------------------------- thread completeness --

impl Harness {
    /// Seed one message into an existing conversation.
    fn seed_in_thread(&self, thread_id: &str, days: i64, subject: &str, body: &str) -> String {
        self.sim()
            .insert_message(
                &MessageSpec::new()
                    .subject(subject)
                    .text(body)
                    .thread_id(thread_id)
                    .received_at(ReceivedAt::AtMs(self.now_ms() - days * DAY_MS)),
            )
            .expect("seeding the fake mailbox")
    }

    async fn open_thread(&self, thread_id: &str) -> (StatusCode, Value) {
        let response = authed_get(
            &self.app,
            &format!("/v1/threads/{thread_id}"),
            Some(&self.token),
        )
        .await;
        let status = response.status();
        (status, response_json(response).await)
    }

    async fn thread_is_marked_complete(&self, thread_id: &str) -> bool {
        sqlx::query_scalar("select complete from threads where account_id = $1 and thread_id = $2")
            .bind(self.account)
            .bind(thread_id)
            .fetch_one(&self.app.db.pool)
            .await
            .unwrap()
    }
}

/// The defect: search hydrates the messages that **matched**, and nothing else.
/// One hit inside a ten-message conversation cached one message of it, and the
/// thread view then served those rows as the conversation - no gap, no marker,
/// nine messages the reader never learned existed.
#[tokio::test]
async fn a_thread_found_by_search_is_completed_when_it_is_opened() {
    let harness = harness().await;
    let thread = "1a00000000000042";

    // A conversation that ran for two years. Only the newest is inside the
    // 30-day window the sync covers.
    harness.seed_in_thread(thread, 700, "Lease renewal", "the original terms");
    let matched = harness.seed_in_thread(thread, 400, "Re: Lease renewal", "the sublet clause");
    harness.seed_in_thread(thread, 3, "Re: Lease renewal", "signing this week");
    harness.sync_thirty_days().await;

    // Exactly one of the three is cached, and it is not the one search finds.
    assert_eq!(harness.cached_gmail_ids().await.len(), 1);

    let found = harness.search("sublet").await;
    assert_eq!(thread_ids(&found), vec![thread.to_owned()]);
    let cached_after_search = harness.cached_gmail_ids().await;
    assert!(
        cached_after_search.contains(&matched),
        "search caches what it matched"
    );
    assert_eq!(
        cached_after_search.len(),
        2,
        "…and only what it matched: the 700-day-old message is still missing"
    );

    // Opening it is what completes it.
    let (status, detail) = harness.open_thread(thread).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        detail["messages"].as_array().unwrap().len(),
        3,
        "the whole conversation, not the part that happened to match: {detail}"
    );
    assert_eq!(detail["partial"], false);
    assert!(harness.thread_is_marked_complete(thread).await);

    // Oldest first, still (`API.md` §2).
    let subjects: Vec<&str> = detail["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["body_text"].as_str().unwrap())
        .collect();
    assert_eq!(
        subjects,
        vec!["the original terms", "the sublet clause", "signing this week"]
    );
}

/// Completing costs one Gmail call, once. A thread already known whole is a
/// local read - otherwise every scroll through a mailbox would spend quota.
#[tokio::test]
async fn a_completed_thread_is_never_asked_about_again() {
    let harness = harness().await;
    let thread = "1a00000000000043";
    harness.seed_in_thread(thread, 2, "Standup", "notes");
    harness.sync_thirty_days().await;

    let (status, _) = harness.open_thread(thread).await;
    assert_eq!(status, StatusCode::OK);
    assert!(harness.thread_is_marked_complete(thread).await);

    let before = harness.sim().calls().len();
    let (status, detail) = harness.open_thread(thread).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["partial"], false);
    assert_eq!(
        harness.sim().calls().len(),
        before,
        "a thread we know is whole must not cost another threads.get"
    );
}

/// EDGE (429/timeout/offline): the rows we hold are still the user's mail, so
/// they are served - but flagged, and the thread is **not** recorded as whole,
/// so the next open tries again. The failure mode this prevents is the quiet
/// one: a truthful-looking thread with messages missing from it.
#[tokio::test]
async fn a_thread_that_cannot_be_completed_is_served_flagged_rather_than_silently_short() {
    use nade_gmail_sim::fault::{Condition, Fault, FaultRule};

    let harness = harness().await;
    let thread = "1a00000000000044";
    harness.seed_in_thread(thread, 500, "Insurance claim", "the original");
    harness.seed_in_thread(thread, 4, "Re: Insurance claim", "the adjuster replied");
    harness.sync_thirty_days().await;

    harness.sim().inject(
        FaultRule::new(Fault::Unavailable {
            retry_after_seconds: None,
        })
        .when(Condition::PathContains("/threads/".to_owned())),
    );

    let (status, detail) = harness.open_thread(thread).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "what we hold is still worth showing: {detail}"
    );
    assert_eq!(
        detail["partial"], true,
        "…but it must not be presented as the whole conversation: {detail}"
    );
    assert_eq!(detail["messages"].as_array().unwrap().len(), 1);
    assert!(
        !harness.thread_is_marked_complete(thread).await,
        "a failed completion must not be recorded as a completed one"
    );
}
