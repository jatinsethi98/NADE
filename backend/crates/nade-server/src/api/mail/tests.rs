//! Read-endpoint tests. Every assertion here is a line of `docs/API.md`.

use axum::http::StatusCode;
use chrono::{DateTime, TimeZone as _, Utc};
use serde_json::Value;
use uuid::Uuid;

use super::*;
use crate::{
    config::Env,
    mail::parse::{ParsedAttachment, ParsedMessage},
    sync::store,
    test_support::{get as authed_get, response_json, test_app, TestApp},
};

// ------------------------------------------------------------- seeding --

async fn connected(app: &TestApp) -> Uuid {
    sqlx::query_scalar(
        "insert into accounts (email, status) values ('jatinsethi98@gmail.com', 'ok') returning id",
    )
    .fetch_one(&app.db.pool)
    .await
    .unwrap()
}

async fn seed_labels(app: &TestApp, account: Uuid, labels: &[(&str, &str, &str)]) {
    for (id, name, kind) in labels {
        sqlx::query("insert into labels (account_id, label_id, name, type) values ($1,$2,$3,$4)")
            .bind(account)
            .bind(id)
            .bind(name)
            .bind(kind)
            .execute(&app.db.pool)
            .await
            .unwrap();
    }
}

/// Gmail's default label set, as the sync would have stored it.
async fn seed_default_labels(app: &TestApp, account: Uuid) {
    seed_labels(
        app,
        account,
        &[
            ("INBOX", "INBOX", "system"),
            ("UNREAD", "UNREAD", "system"),
            ("SENT", "SENT", "system"),
            ("SPAM", "SPAM", "system"),
            ("TRASH", "TRASH", "system"),
            ("IMPORTANT", "IMPORTANT", "system"),
            ("CATEGORY_PERSONAL", "CATEGORY_PERSONAL", "system"),
            ("CATEGORY_UPDATES", "CATEGORY_UPDATES", "system"),
            ("CATEGORY_SOCIAL", "CATEGORY_SOCIAL", "system"),
            ("CATEGORY_PROMOTIONS", "CATEGORY_PROMOTIONS", "system"),
            ("CATEGORY_FORUMS", "CATEGORY_FORUMS", "system"),
            ("STARRED", "STARRED", "system"),
            ("Label_12", "To Reply", "user"),
            ("Label_18", "Awaiting Reply", "user"),
            ("Label_24", "[Notion]", "user"),
            ("Label_99", "[Gmail]All Mail", "user"),
        ],
    )
    .await;
}

struct Seed<'a> {
    gmail_id: &'a str,
    thread_id: &'a str,
    ts: DateTime<Utc>,
    labels: &'a [&'a str],
    subject: &'a str,
    from_name: &'a str,
    from_email: &'a str,
    body_text: &'a str,
    body_html: Option<&'a str>,
    attachments: Vec<ParsedAttachment>,
}

impl<'a> Seed<'a> {
    fn new(gmail_id: &'a str, thread_id: &'a str, ts: DateTime<Utc>) -> Self {
        Self {
            gmail_id,
            thread_id,
            ts,
            labels: &["INBOX", "CATEGORY_PERSONAL"],
            subject: "Staff Product Designer at Kettle",
            from_name: "Priya Raghavan",
            from_email: "priya@kettle.com",
            body_text: "Hi Jatin - I came across your work on the Northbound reader.",
            body_html: None,
            attachments: Vec::new(),
        }
    }
}

async fn seed_message(app: &TestApp, account: Uuid, seed: Seed<'_>) {
    let parsed = ParsedMessage {
        subject: seed.subject.to_owned(),
        from_name: seed.from_name.to_owned(),
        from_email: seed.from_email.to_owned(),
        to: vec!["jatinsethi98@gmail.com".to_owned()],
        cc: Vec::new(),
        date: Some(seed.ts),
        body_text: seed.body_text.to_owned(),
        body_html: seed.body_html.map(str::to_owned),
        attachments: seed.attachments,
    };
    let message = crate::gmail::types::GmailMessage {
        id: seed.gmail_id.to_owned(),
        thread_id: Some(seed.thread_id.to_owned()),
        label_ids: seed.labels.iter().map(|l| (*l).to_owned()).collect(),
        internal_date: Some(seed.ts.timestamp_millis().to_string()),
        ..Default::default()
    };

    let row = store::IngestRow::parsed(&message, parsed);
    let mut tx = app.db.pool.begin().await.unwrap();
    store::upsert_message(&mut tx, account, &row).await.unwrap();
    store::refresh_thread(&mut tx, account, seed.thread_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

fn at(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_786_950_724 + seconds, 0).unwrap()
}

/// A paired device plus a connected, seeded account.
async fn app_with_mail() -> (TestApp, String, Uuid) {
    let app = test_app(Env::Prod).await;
    let token = app.device_token().await;
    let account = connected(&app).await;
    seed_default_labels(&app, account).await;
    (app, token, account)
}

async fn json(app: &TestApp, path: &str, token: &str) -> Value {
    let response = authed_get(app, path, Some(token)).await;
    assert_eq!(response.status(), StatusCode::OK, "GET {path}");
    response_json(response).await
}

/// The key **set** of a JSON object. JSON objects are unordered and
/// `serde_json::Value` reads them into a `BTreeMap`, so asserting on order here
/// would test serde rather than the contract - what matters is that every field
/// `API.md` promises is present and nothing else is.
fn keys(value: &Value) -> Vec<String> {
    let mut names: Vec<String> = value
        .as_object()
        .unwrap_or_else(|| panic!("expected an object, got {value}"))
        .keys()
        .cloned()
        .collect();
    names.sort();
    names
}

fn sorted(names: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    out.sort();
    out
}

// --------------------------------------------------------------- /me --

/// Criterion O1.
#[tokio::test]
async fn me_reports_the_account_and_its_status() {
    let app = test_app(Env::Prod).await;
    let token = app.device_token().await;

    // EDGE (empty input): nothing connected yet. `/me` still answers, and the
    // status drives the client straight to the sign-in row in Settings.
    let body = json(&app, "/v1/me", &token).await;
    assert_eq!(
        body,
        serde_json::json!({"email": "", "status": "needs_reauth"})
    );

    let account = connected(&app).await;
    let body = json(&app, "/v1/me", &token).await;
    assert_eq!(
        body,
        serde_json::json!({"email": "jatinsethi98@gmail.com", "status": "ok"})
    );
    assert_eq!(
        body.as_object().unwrap().len(),
        2,
        "exactly two fields, both always present"
    );

    sqlx::query("update accounts set status = 'needs_reauth' where id = $1")
        .bind(account)
        .execute(&app.db.pool)
        .await
        .unwrap();
    let body = json(&app, "/v1/me", &token).await;
    assert_eq!(body["status"], "needs_reauth");
}

// -------------------------------------------------------- /mailboxes --

/// Criterion O2.
#[tokio::test]
async fn mailboxes_are_the_whitelist_then_user_labels() {
    let (app, token, _) = app_with_mail().await;
    let body = json(&app, "/v1/mailboxes", &token).await;

    assert!(
        body.as_object().unwrap().get("next_cursor").is_none(),
        "a bounded collection carries no cursor field at all (API.md §0)"
    );

    let names: Vec<&str> = body["mailboxes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|mailbox| mailbox["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            // The eight, in the contract's order, with the contract's names.
            "Inbox",
            "Primary",
            "Updates",
            "Social",
            "Promotions",
            "Forums",
            "Starred",
            "Sent",
            // Then user labels, by raw byte value of the name: '[' is 0x5B,
            // which sorts after every uppercase letter.
            "Awaiting Reply",
            "To Reply",
            "[Notion]",
        ]
    );

    let ids: Vec<&str> = body["mailboxes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|mailbox| mailbox["id"].as_str().unwrap())
        .collect();
    for hidden in ["UNREAD", "IMPORTANT", "SPAM", "TRASH", "Label_99"] {
        assert!(!ids.contains(&hidden), "{hidden} must not be a mailbox");
    }
    assert_eq!(body["mailboxes"][0]["kind"], "system");
    assert_eq!(body["mailboxes"][8]["kind"], "user");

    // Every field is present on every row, with the right types.
    for mailbox in body["mailboxes"].as_array().unwrap() {
        assert_eq!(
            keys(mailbox),
            sorted(&["id", "name", "kind", "unread", "total"])
        );
        assert!(mailbox["unread"].is_i64() && mailbox["total"].is_i64());
    }
}

/// Criterion O3.
#[tokio::test]
async fn mailbox_counts_are_threads_not_messages() {
    let (app, token, account) = app_with_mail().await;

    // One thread, three messages, one of them unread.
    for (index, labels) in [
        (0, &["INBOX", "CATEGORY_PERSONAL"][..]),
        (1, &["INBOX", "CATEGORY_PERSONAL"][..]),
        (2, &["INBOX", "CATEGORY_PERSONAL", "UNREAD"][..]),
    ] {
        seed_message(
            &app,
            account,
            Seed {
                labels,
                ..Seed::new(&format!("m{index}"), "thread-a", at(index))
            },
        )
        .await;
    }
    // A second thread, read.
    seed_message(
        &app,
        account,
        Seed {
            labels: &["INBOX", "Label_12"],
            ..Seed::new("m9", "thread-b", at(9))
        },
    )
    .await;

    let body = json(&app, "/v1/mailboxes", &token).await;
    let by_id: std::collections::HashMap<&str, &Value> = body["mailboxes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|mailbox| (mailbox["id"].as_str().unwrap(), mailbox))
        .collect();

    assert_eq!(by_id["INBOX"]["total"], 2, "two threads, not four messages");
    assert_eq!(
        by_id["INBOX"]["unread"], 1,
        "a thread counts as unread when ANY message in it is"
    );
    assert_eq!(by_id["Label_12"]["total"], 1);
    assert_eq!(by_id["Label_12"]["unread"], 0);
    // A mailbox with nothing in the window is still listed, at zero.
    assert_eq!(by_id["SENT"]["total"], 0);
}

#[tokio::test]
async fn mailboxes_are_empty_before_the_first_sync() {
    let app = test_app(Env::Prod).await;
    let token = app.device_token().await;
    let body = json(&app, "/v1/mailboxes", &token).await;
    assert_eq!(body["mailboxes"].as_array().unwrap().len(), 0);
}

// ----------------------------------------------------------- /threads --

/// Criteria O4 + O5.
#[tokio::test]
async fn thread_list_paginates_by_keyset() {
    let (app, token, account) = app_with_mail().await;
    for index in 0..12 {
        seed_message(
            &app,
            account,
            Seed {
                subject: &format!("Thread {index:02}"),
                ..Seed::new(&format!("m{index:02}"), &format!("t{index:02}"), at(index))
            },
        )
        .await;
    }

    let first = json(&app, "/v1/mailboxes/INBOX/threads?limit=5", &token).await;
    let ids: Vec<&str> = first["threads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|thread| thread["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["t11", "t10", "t09", "t08", "t07"],
        "sorted by ts descending"
    );
    let cursor = first["next_cursor"]
        .as_str()
        .expect("a next page")
        .to_owned();

    let second = json(
        &app,
        &format!("/v1/mailboxes/INBOX/threads?limit=5&cursor={cursor}"),
        &token,
    )
    .await;
    let ids: Vec<&str> = second["threads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|thread| thread["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["t06", "t05", "t04", "t03", "t02"]);

    // EDGE (pagination boundary): the last page carries `next_cursor: null`.
    let cursor = second["next_cursor"].as_str().unwrap().to_owned();
    let last = json(
        &app,
        &format!("/v1/mailboxes/INBOX/threads?limit=5&cursor={cursor}"),
        &token,
    )
    .await;
    let ids: Vec<&str> = last["threads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|thread| thread["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["t01", "t00"]);
    assert!(last["next_cursor"].is_null());

    // `limit` is clamped, never trusted.
    let big = json(&app, "/v1/mailboxes/INBOX/threads?limit=100000", &token).await;
    assert_eq!(big["threads"].as_array().unwrap().len(), 12);
    assert!(big["next_cursor"].is_null());
}

/// Criterion O5 - the reason the cursor is keyset and not offset.
#[tokio::test]
async fn a_row_inserted_mid_scroll_neither_duplicates_nor_skips() {
    let (app, token, account) = app_with_mail().await;
    for index in 0..6 {
        seed_message(
            &app,
            account,
            Seed::new(&format!("m{index}"), &format!("t{index}"), at(index)),
        )
        .await;
    }

    let first = json(&app, "/v1/mailboxes/INBOX/threads?limit=3", &token).await;
    let seen: Vec<String> = first["threads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|thread| thread["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(seen, vec!["t5", "t4", "t3"]);
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();

    // A brand new message arrives at the top while the user is scrolling. With
    // `offset=3` this would push `t2` onto page two and show it twice; with a
    // keyset cursor the second page is unaffected.
    seed_message(&app, account, Seed::new("m-new", "t-new", at(1_000))).await;

    let second = json(
        &app,
        &format!("/v1/mailboxes/INBOX/threads?limit=3&cursor={cursor}"),
        &token,
    )
    .await;
    let next: Vec<String> = second["threads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|thread| thread["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(next, vec!["t2", "t1", "t0"], "no duplicate, no skip");

    let all: std::collections::BTreeSet<String> =
        seen.into_iter().chain(next.into_iter()).collect();
    assert_eq!(all.len(), 6);
}

/// Criterion O6.
#[tokio::test]
async fn an_unknown_cursor_is_a_bad_request() {
    let (app, token, account) = app_with_mail().await;
    seed_message(&app, account, Seed::new("m0", "t0", at(0))).await;

    for bad in ["garbage", "eyJvZmZzZXQiOjUwfQ", "%20"] {
        let response = authed_get(
            &app,
            &format!("/v1/mailboxes/INBOX/threads?cursor={bad}"),
            Some(&token),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "cursor {bad:?} must be refused, not silently reset to page one"
        );
        assert_eq!(
            response_json(response).await["error"]["code"],
            "bad_request"
        );
    }
}

/// EDGE (empty input): an unknown mailbox and an empty one are the same shape,
/// and neither is a 404.
#[tokio::test]
async fn an_empty_mailbox_is_an_empty_array_not_a_404() {
    let (app, token, _) = app_with_mail().await;
    for path in [
        "/v1/mailboxes/SENT/threads",
        "/v1/mailboxes/Label_nope/threads",
    ] {
        let body = json(&app, path, &token).await;
        assert_eq!(body["threads"].as_array().unwrap().len(), 0);
        assert!(body["next_cursor"].is_null());
    }
}

#[tokio::test]
async fn a_thread_row_matches_the_contract_field_for_field() {
    let (app, token, account) = app_with_mail().await;
    seed_message(
        &app,
        account,
        Seed {
            from_name: "",
            subject: "",
            ..Seed::new("m0", "18f2a1b3c4d5e6f7", at(0))
        },
    )
    .await;

    let body = json(&app, "/v1/mailboxes/INBOX/threads", &token).await;
    let thread = &body["threads"][0];
    assert_eq!(
        keys(thread),
        sorted(&[
            "id",
            "subject",
            "snippet",
            "from_name",
            "from_email",
            "ts",
            "unread",
            "msg_count",
            "agent_note"
        ])
    );
    assert_eq!(thread["id"], "18f2a1b3c4d5e6f7");
    assert_eq!(
        thread["subject"], "",
        "\"\" when the mail has none - never null"
    );
    assert_eq!(
        thread["from_name"], "",
        "\"\" rather than null; the server does not invent a name"
    );
    assert_eq!(thread["from_email"], "priya@kettle.com");
    assert_eq!(thread["msg_count"], 1);
    assert_eq!(thread["unread"], false);
    assert!(thread["agent_note"].is_null(), "no agent runs until P4");
    assert_eq!(
        thread["ts"].as_str().unwrap(),
        at(0).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    );
    assert!(
        !thread["ts"].as_str().unwrap().contains('.'),
        "second precision, never milliseconds"
    );
    assert!(thread["snippet"].as_str().unwrap().len() <= 200);
}

/// `agent_note` is whatever the run stored, verbatim - the string the row
/// renders.
///
/// **It is a column, not a key in `data`.** This test used to seed
/// `data.agent_note`, matching a P2 comment that promised P5 would write it
/// there. It could not: `data` is served verbatim and
/// `docs/contract/validate.py`'s `FEED_DATA` is an exact key set, so a card
/// carrying an extra key would have failed the contract the day `/feed` was
/// mounted. Migration 0006 gave the note its own column.
#[tokio::test]
async fn an_agent_note_is_passed_through_verbatim() {
    let (app, token, account) = app_with_mail().await;
    seed_message(&app, account, Seed::new("m0", "t0", at(0))).await;

    sqlx::query(
        "insert into feed_items \
             (account_id, kind, title, body, data, status, thread_id, agent_note) \
         values ($1, 'approval', 'Job Search Tracker', 'body', $2::jsonb, 'new', 't0', $3)",
    )
    .bind(account)
    .bind(serde_json::json!({
        "action": "write_note", "action_label": "Save note",
        "note_title": "Kettle", "note_id": uuid::Uuid::nil(), "thread_id": "t0"
    }))
    .bind("Job Search Tracker · a note to approve")
    .execute(&app.db.pool)
    .await
    .unwrap();

    let body = json(&app, "/v1/mailboxes/INBOX/threads", &token).await;
    assert_eq!(
        body["threads"][0]["agent_note"],
        "Job Search Tracker · a note to approve"
    );
}

/// `API.md` §2's agent cards, and the four ways a run reaches a thread.
///
/// Deliberately one test over all four rather than four tests over one each:
/// the failure mode that matters is a `union` branch that silently matches
/// nothing, and only a case that would be found by *no other branch* proves a
/// given branch works.
#[tokio::test]
async fn agent_cards_find_a_run_by_every_route_a_run_reaches_a_thread() {
    let (app, token, account) = app_with_mail().await;
    seed_message(&app, account, Seed::new("m0", "t0", at(0))).await;
    let agent: uuid::Uuid = sqlx::query_scalar(
        "insert into agents (account_id, name, nl_definition, status) \
         values ($1, 'Job Search Tracker', 'note it', 'published') returning id",
    )
    .bind(account)
    .fetch_one(&app.db.pool)
    .await
    .unwrap();

    async fn run(
        app: &crate::test_support::TestApp,
        account: uuid::Uuid,
        agent: uuid::Uuid,
        status: &str,
        trigger: &str,
        trigger_ref: Option<&str>,
    ) -> uuid::Uuid {
        sqlx::query_scalar(
            "insert into agent_runs (agent_id, account_id, trigger_kind, trigger_ref, status, \
                                     summary) \
             values ($1, $2, $3, $4, $5, 'Two next steps found.') returning id",
        )
        .bind(agent)
        .bind(account)
        .bind(trigger)
        .bind(trigger_ref)
        .bind(status)
        .fetch_one(&app.db.pool)
        .await
        .unwrap()
    }

    // 1. a mail trigger on a message *in* the thread. `trigger_ref` is the
    //    message id (`API.md` §6), so this only works through `messages`.
    let by_trigger = run(&app, account, agent, "pending_approval", "mail", Some("m0")).await;
    // 2. a card naming the thread.
    let by_card = run(&app, account, agent, "done", "manual", None).await;
    sqlx::query(
        "insert into feed_items (account_id, run_id, kind, title, body, data, status, thread_id) \
         values ($1, $2, 'info', 'Job Search Tracker', 'Saved.', $3::jsonb, 'resolved', 't0')",
    )
    .bind(account)
    .bind(by_card)
    .bind(crate::agents::feed::info_data(None, None, Some("t0")))
    .execute(&app.db.pool)
    .await
    .unwrap();
    // 3. a note it wrote.
    let by_note = run(&app, account, agent, "done", "manual", None).await;
    sqlx::query(
        "insert into notes (id, account_id, run_id, thread_id, title) \
         values (gen_random_uuid(), $1, $2, 't0', 'Kettle')",
    )
    .bind(account)
    .bind(by_note)
    .execute(&app.db.pool)
    .await
    .unwrap();
    // 4. a draft it prepared.
    let by_draft = run(&app, account, agent, "failed", "manual", None).await;
    sqlx::query(
        "insert into drafts (id, account_id, run_id, thread_id, subject) \
         values (gen_random_uuid(), $1, $2, 't0', 'Re: Kettle')",
    )
    .bind(account)
    .bind(by_draft)
    .execute(&app.db.pool)
    .await
    .unwrap();

    // And one run that touches a *different* thread, which must not appear.
    let elsewhere = run(&app, account, agent, "done", "manual", None).await;
    sqlx::query(
        "insert into notes (id, account_id, run_id, thread_id, title) \
         values (gen_random_uuid(), $1, $2, 'other', 'Elsewhere')",
    )
    .bind(account)
    .bind(elsewhere)
    .execute(&app.db.pool)
    .await
    .unwrap();

    let body = json(&app, "/v1/threads/t0", &token).await;
    let cards = body["agent_cards"].as_array().expect("agent_cards");
    let ids: Vec<&str> = cards
        .iter()
        .map(|card| card["run_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 4, "{cards:#?}");
    for wanted in [by_trigger, by_card, by_note, by_draft] {
        assert!(
            ids.contains(&wanted.to_string().as_str()),
            "{wanted} missing"
        );
    }
    assert!(
        !ids.contains(&elsewhere.to_string().as_str()),
        "another thread's run"
    );

    // Newest run first (`API.md` §2), and the card that has one names it.
    assert_eq!(ids[0], by_draft.to_string(), "newest first");
    let carded = cards
        .iter()
        .find(|card| card["run_id"] == by_card.to_string())
        .unwrap();
    assert!(carded["feed_item_id"].is_string());
    assert_eq!(carded["agent_name"], "Job Search Tracker");
    assert_eq!(carded["summary"], "Two next steps found.");
    // A run that never asked for anything has nothing to tap through to.
    let bare = cards
        .iter()
        .find(|card| card["run_id"] == by_note.to_string())
        .unwrap();
    assert_eq!(bare["feed_item_id"], serde_json::json!(null));
}

/// `AgentCard.summary` is non-nullable on the wire and `agent_runs.summary` is
/// not written for a failed run — which would render an empty box in the state
/// DESIGN §1f gives a status string for.
#[tokio::test]
async fn a_failed_runs_card_says_why_instead_of_rendering_nothing() {
    let (app, token, account) = app_with_mail().await;
    seed_message(&app, account, Seed::new("m0", "t0", at(0))).await;
    let agent: uuid::Uuid = sqlx::query_scalar(
        "insert into agents (account_id, name, nl_definition, status) \
         values ($1, 'Tracker', 'note it', 'published') returning id",
    )
    .bind(account)
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into agent_runs (agent_id, account_id, trigger_kind, trigger_ref, status, error) \
         values ($1, $2, 'mail', 'm0', 'failed', 'Gmail was unavailable.')",
    )
    .bind(agent)
    .bind(account)
    .execute(&app.db.pool)
    .await
    .unwrap();

    let body = json(&app, "/v1/threads/t0", &token).await;
    assert_eq!(body["agent_cards"][0]["status"], "failed");
    assert_eq!(body["agent_cards"][0]["summary"], "Gmail was unavailable.");
}

// ------------------------------------------------------ /threads/{id} --

/// Criterion O7.
#[tokio::test]
async fn thread_detail_matches_the_contract() {
    let (app, token, account) = app_with_mail().await;

    seed_message(
        &app,
        account,
        Seed {
            subject: "Staff Product Designer at Kettle",
            body_text: "Hi Jatin - the first message.",
            ..Seed::new("older", "18f2a1b3c4d5e6f7", at(0))
        },
    )
    .await;
    seed_message(
        &app,
        account,
        Seed {
            labels: &["INBOX", "UNREAD", "Label_12"],
            subject: "Re: Staff Product Designer at Kettle",
            body_text: "And the second.",
            body_html: Some("<div><p>And the second.</p></div>"),
            attachments: vec![ParsedAttachment {
                att_id: "ANGjdJ_qk9mR0xT2wPmC5nQ".to_owned(),
                name: "Kettle — Staff Designer.pdf".to_owned(),
                mime: "application/pdf".to_owned(),
                size_bytes: 245_760,
                content_id: None,
                inline: false,
                ordinal: 0,
            }],
            ..Seed::new("newer", "18f2a1b3c4d5e6f7", at(60))
        },
    )
    .await;

    let body = json(&app, "/v1/threads/18f2a1b3c4d5e6f7", &token).await;
    assert_eq!(
        keys(&body),
        sorted(&[
            "id",
            "subject",
            "mailbox_name",
            "account_email",
            "messages",
            "agent_cards",
            "partial"
        ])
    );
    assert_eq!(body["id"], "18f2a1b3c4d5e6f7");
    assert_eq!(body["account_email"], "jatinsethi98@gmail.com");
    assert_eq!(
        body["mailbox_name"], "To Reply",
        "the user's own label, not the Inbox every thread is in"
    );
    assert_eq!(body["agent_cards"].as_array().unwrap().len(), 0);

    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[0]["gmail_id"], "older",
        "messages are ordered oldest first"
    );
    assert_eq!(messages[1]["gmail_id"], "newer");

    assert_eq!(
        keys(&messages[0]),
        sorted(&[
            "gmail_id",
            "from_name",
            "from_email",
            "to",
            "cc",
            "ts",
            "body_text",
            "body_html",
            "label_ids",
            "attachments"
        ]),
        "there is NO `id` field on a message; gmail_id is the identity"
    );
    assert!(
        messages[0].get("id").is_none(),
        "the database's bigint primary key never leaves the server"
    );

    assert_eq!(messages[0]["body_text"], "Hi Jatin - the first message.");
    assert!(
        messages[0]["body_html"].is_null(),
        "null when there is no text/html part - it is not a rendering of the text"
    );
    assert_eq!(
        messages[1]["body_html"],
        "<div><p>And the second.</p></div>"
    );
    assert_eq!(messages[0]["cc"].as_array().unwrap().len(), 0);
    assert_eq!(messages[0]["to"][0], "jatinsethi98@gmail.com");

    let attachment = &messages[1]["attachments"][0];
    assert_eq!(
        keys(attachment),
        sorted(&["id", "name", "mime", "size", "inline"])
    );
    assert_eq!(attachment["id"], "ANGjdJ_qk9mR0xT2wPmC5nQ");
    assert_eq!(attachment["name"], "Kettle — Staff Designer.pdf");
    assert_eq!(attachment["size"], 245_760);
    assert_eq!(attachment["inline"], false);
    assert_eq!(messages[0]["attachments"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn an_unknown_thread_is_a_404() {
    let (app, token, account) = app_with_mail().await;
    seed_message(&app, account, Seed::new("m0", "t0", at(0))).await;

    let response = authed_get(&app, "/v1/threads/not-a-thread", Some(&token)).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(response_json(response).await["error"]["code"], "not_found");
}

/// EDGE (empty input): a genuinely empty body is `""`, and the field is still
/// there.
#[tokio::test]
async fn an_empty_body_is_an_empty_string() {
    let (app, token, account) = app_with_mail().await;
    seed_message(
        &app,
        account,
        Seed {
            body_text: "",
            ..Seed::new("m0", "t0", at(0))
        },
    )
    .await;

    let body = json(&app, "/v1/threads/t0", &token).await;
    assert_eq!(body["messages"][0]["body_text"], "");
    assert!(!body["messages"][0]["body_text"].is_null());
}

// ------------------------------------------------------------ /search --
//
// `GET /search` no longer reads the database at all: `docs/SEARCH.md` made
// Gmail the index, and the endpoint is a wrapper over `crate::search::search`.
// Everything it does - finding mail outside the sync window, hydrating the
// misses, keeping Gmail's ranking, refusing a query Gmail would swallow - is
// tested in `search::tests` against a fake Gmail with a real query parser.
// What is left here is the contract this module still owns.

/// Criterion O8 - EDGE (empty input). The caps live in the validator now, but
/// they are still the endpoint's promise, so they are still asserted here.
#[tokio::test]
async fn search_refuses_an_empty_or_oversized_query() {
    let (app, token, _) = app_with_mail().await;

    for query in ["", "q=", "q=%20%20%20"] {
        let response = authed_get(&app, &format!("/v1/search?{query}"), Some(&token)).await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "an empty q must not mean \"everything\": {query:?}"
        );
        assert_eq!(
            response_json(response).await["error"]["code"],
            "bad_request"
        );
    }

    let long = "a".repeat(513);
    let response = authed_get(&app, &format!("/v1/search?q={long}"), Some(&token)).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

/// The endpoint is a wrapper and nothing more: no second query language, no
/// second ranking, no second cursor. If this ever grows a database read again,
/// the 0.78% index is back.
#[test]
fn the_search_endpoint_delegates_rather_than_querying() {
    let source =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/api/mail.rs")).unwrap();
    let handler = source
        .split("pub async fn search(")
        .nth(1)
        .expect("the search handler")
        .split("\n/// ")
        .next()
        .unwrap();

    assert!(
        handler.contains("crate::search::search"),
        "the handler must delegate:\n{handler}"
    );
    assert!(
        !handler.contains("sqlx::"),
        "search reads Gmail, not the cache:\n{handler}"
    );
}

// -------------------------------------------------- attachment proxy --

/// Seed a `gmail_tokens` row the real `TokenStore` can read back, so the proxy
/// gets an access token without a live Google.
async fn seed_token(app: &TestApp, account: Uuid) {
    let cipher = app.gmail_cipher();
    sqlx::query(
        "insert into gmail_tokens (account_id, access_token, access_expiry, refresh_token, scopes) \
         values ($1, $2, now() + interval '1 hour', $3, array['gmail.readonly'])",
    )
    .bind(account)
    .bind(cipher.encrypt("ya29.proxy-token").unwrap())
    .bind(cipher.encrypt("1//refresh").unwrap())
    .execute(&app.db.pool)
    .await
    .unwrap();
}

async fn app_with_attachment(name: &str, size: i64) -> (TestApp, String, wiremock::MockServer) {
    use wiremock::MockServer;

    let server = MockServer::start().await;
    let mut app = test_app(Env::Prod).await;
    app.set_gmail_base(&server.uri());
    let token = app.device_token().await;
    let account = connected(&app).await;
    seed_default_labels(&app, account).await;
    seed_token(&app, account).await;

    seed_message(
        &app,
        account,
        Seed {
            attachments: vec![ParsedAttachment {
                att_id: "AttOne".to_owned(),
                name: name.to_owned(),
                mime: "application/pdf".to_owned(),
                size_bytes: size,
                content_id: None,
                inline: false,
                ordinal: 0,
            }],
            ..Seed::new("18f2a1b3c4d5e6f7", "t0", at(0))
        },
    )
    .await;

    (app, token, server)
}

/// Criteria O9 + O11.
#[tokio::test]
async fn attachment_proxy_streams_from_gmail_with_an_rfc5987_filename() {
    use base64::Engine as _;
    use wiremock::{
        matchers::{method, path, path_regex},
        Mock, ResponseTemplate,
    };

    let bytes = b"%PDF-1.7 the real thing";
    let name = "Kettle — Staff Designer 🚀 配送.pdf";
    let (app, token, server) = app_with_attachment(name, i64::try_from(bytes.len()).unwrap()).await;

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages/18f2a1b3c4d5e6f7"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"id":"18f2a1b3c4d5e6f7","payload":{{"mimeType":"multipart/mixed","parts":[
                     {{"mimeType":"text/plain","body":{{"size":10}}}},
                     {{"mimeType":"application/pdf","filename":"{name}",
                       "body":{{"attachmentId":"ANGjdJ_qk9mR0xT2wPmC5nQ","size":{}}}}}]}}}}"#,
                    bytes.len()
                )
                .into_bytes(),
                "application/json",
            ),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(
            r"^/gmail/v1/users/me/messages/.+/attachments/.+$",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"size":{},"data":"{}"}}"#,
                    bytes.len(),
                    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
                )
                .into_bytes(),
                "application/json",
            ),
        )
        .mount(&server)
        .await;

    let response = authed_get(
        &app,
        "/v1/messages/18f2a1b3c4d5e6f7/attachments/AttOne",
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let disposition = response
        .headers()
        .get(axum::http::header::CONTENT_DISPOSITION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(disposition.starts_with("attachment; "), "{disposition}");
    assert!(
        disposition.contains("filename*=UTF-8''"),
        "the real name needs RFC 5987: {disposition}"
    );
    assert!(
        disposition.contains("%F0%9F%9A%80"),
        "the emoji must be percent-encoded, not raw: {disposition}"
    );
    assert!(
        disposition.is_ascii(),
        "a header value must be ASCII: {disposition}"
    );

    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .unwrap(),
        "application/pdf"
    );
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CACHE_CONTROL)
            .unwrap(),
        "no-store, private",
        "the bytes are the user's mail; we are a proxy, not a cache"
    );

    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    assert_eq!(body.as_ref(), bytes);

    // Nothing was stored.
    let stored: i64 = sqlx::query_scalar(
        "select count(*) from information_schema.columns \
          where table_name = 'attachments' and column_name in ('data','bytes','content')",
    )
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    assert_eq!(stored, 0);
}

/// A part small enough that Gmail inlines it needs no second call.
#[tokio::test]
async fn a_small_attachment_is_served_from_the_inline_body() {
    use base64::Engine as _;
    use wiremock::{
        matchers::{method, path},
        Mock, ResponseTemplate,
    };

    let bytes = b"tiny";
    let (app, token, server) = app_with_attachment("tiny.pdf", 4).await;
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages/18f2a1b3c4d5e6f7"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"id":"18f2a1b3c4d5e6f7","payload":{{"parts":[
                     {{"mimeType":"application/pdf","filename":"tiny.pdf",
                       "body":{{"size":4,"data":"{}"}}}}]}}}}"#,
                    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
                )
                .into_bytes(),
                "application/json",
            ),
        )
        .mount(&server)
        .await;

    let response = authed_get(
        &app,
        "/v1/messages/18f2a1b3c4d5e6f7/attachments/AttOne",
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    assert_eq!(body.as_ref(), bytes);
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "an inline part needs no attachments.get"
    );
}

/// Two attachments, one name, one size - and before this, both downloads
/// returned the **first** one's bytes. Deterministically, silently, with a 200.
///
/// `format=raw` carries no `attachmentId`, so the stored row has to be matched
/// back to a live part by hand; matching on name and size alone cannot tell
/// identical twins apart. `ordinal` is the tie-break, and it works because our
/// attachment list and Gmail's part tree agree on *order* even though they are
/// built by different parsers (backend/DECISIONS.md D14).
#[tokio::test]
async fn two_attachments_with_one_name_and_size_stay_distinct() {
    use base64::Engine as _;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    // Same length, so size cannot separate them either.
    let first = b"FIRST-ONE";
    let second = b"SECOND!!!";
    assert_eq!(first.len(), second.len());

    let server = MockServer::start().await;
    let mut app = test_app(Env::Prod).await;
    app.set_gmail_base(&server.uri());
    let token = app.device_token().await;
    let account = connected(&app).await;
    seed_default_labels(&app, account).await;
    seed_token(&app, account).await;

    let twin = |att_id: &str, ordinal: i32| ParsedAttachment {
        att_id: att_id.to_owned(),
        name: "invoice.pdf".to_owned(),
        mime: "application/pdf".to_owned(),
        size_bytes: 9,
        content_id: None,
        inline: false,
        ordinal,
    };
    seed_message(
        &app,
        account,
        Seed {
            attachments: vec![twin("AttA", 0), twin("AttB", 1)],
            ..Seed::new("18f2a1b3c4d5e6f7", "t0", at(0))
        },
    )
    .await;

    let b64 = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages/18f2a1b3c4d5e6f7"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"id":"18f2a1b3c4d5e6f7","payload":{{"mimeType":"multipart/mixed","parts":[
                     {{"mimeType":"application/pdf","filename":"invoice.pdf",
                       "body":{{"size":9,"data":"{}"}}}},
                     {{"mimeType":"application/pdf","filename":"invoice.pdf",
                       "body":{{"size":9,"data":"{}"}}}}]}}}}"#,
                    b64(first),
                    b64(second)
                )
                .into_bytes(),
                "application/json",
            ),
        )
        .mount(&server)
        .await;

    for (att_id, expected) in [("AttA", first.as_slice()), ("AttB", second.as_slice())] {
        let response = authed_get(
            &app,
            &format!("/v1/messages/18f2a1b3c4d5e6f7/attachments/{att_id}"),
            Some(&token),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK, "{att_id}");
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(
            body.as_ref(),
            expected,
            "{att_id} served the wrong file's bytes"
        );
    }
}

/// EDGE (the message changed under us): we hold two look-alikes, Gmail now has
/// one. The second must 404 rather than quietly serve the first - a wrong file
/// with a 200 is worse than an honest miss.
#[tokio::test]
async fn a_twin_that_gmail_no_longer_has_is_a_404_not_the_other_twin() {
    use base64::Engine as _;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    let server = MockServer::start().await;
    let mut app = test_app(Env::Prod).await;
    app.set_gmail_base(&server.uri());
    let token = app.device_token().await;
    let account = connected(&app).await;
    seed_default_labels(&app, account).await;
    seed_token(&app, account).await;

    let twin = |att_id: &str, ordinal: i32| ParsedAttachment {
        att_id: att_id.to_owned(),
        name: "invoice.pdf".to_owned(),
        mime: "application/pdf".to_owned(),
        size_bytes: 9,
        content_id: None,
        inline: false,
        ordinal,
    };
    seed_message(
        &app,
        account,
        Seed {
            attachments: vec![twin("AttA", 0), twin("AttB", 1)],
            ..Seed::new("18f2a1b3c4d5e6f7", "t0", at(0))
        },
    )
    .await;

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages/18f2a1b3c4d5e6f7"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"id":"18f2a1b3c4d5e6f7","payload":{{"mimeType":"multipart/mixed","parts":[
                     {{"mimeType":"application/pdf","filename":"invoice.pdf",
                       "body":{{"size":9,"data":"{}"}}}}]}}}}"#,
                    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"FIRST-ONE")
                )
                .into_bytes(),
                "application/json",
            ),
        )
        .mount(&server)
        .await;

    let response = authed_get(
        &app,
        "/v1/messages/18f2a1b3c4d5e6f7/attachments/AttB",
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// Criterion O9 - the 25 MB cap, refused before any Gmail quota is spent.
#[tokio::test]
async fn attachment_proxy_refuses_over_25mb() {
    let (app, token, server) = app_with_attachment("huge.pdf", MAX_ATTACHMENT_BYTES + 1).await;

    let response = authed_get(
        &app,
        "/v1/messages/18f2a1b3c4d5e6f7/attachments/AttOne",
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "payload_too_large"
    );
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "refuse before spending Gmail quota on bytes we would throw away"
    );
}

/// Criterion O10.
#[tokio::test]
async fn attachment_proxy_404s_when_gmail_forgot_the_message() {
    use wiremock::{
        matchers::{method, path},
        Mock, ResponseTemplate,
    };

    let (app, token, server) = app_with_attachment("gone.pdf", 100).await;
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages/18f2a1b3c4d5e6f7"))
        .respond_with(ResponseTemplate::new(404).set_body_raw(
            br#"{"error":{"code":404,"message":"Not Found"}}"#.to_vec(),
            "application/json",
        ))
        .mount(&server)
        .await;

    let response = authed_get(
        &app,
        "/v1/messages/18f2a1b3c4d5e6f7/attachments/AttOne",
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // And an attachment we never stored is a 404 without touching Gmail at all.
    let response = authed_get(
        &app,
        "/v1/messages/18f2a1b3c4d5e6f7/attachments/NoSuchAttachment",
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// Criterion O13.
#[tokio::test]
async fn an_upstream_failure_is_a_502_not_a_500() {
    use wiremock::{
        matchers::{method, path},
        Mock, ResponseTemplate,
    };

    let (app, token, server) = app_with_attachment("flaky.pdf", 100).await;
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages/18f2a1b3c4d5e6f7"))
        .respond_with(ResponseTemplate::new(503).set_body_raw(
            br#"{"error":{"code":503,"message":"Backend Error"}}"#.to_vec(),
            "application/json",
        ))
        .mount(&server)
        .await;

    let response = authed_get(
        &app,
        "/v1/messages/18f2a1b3c4d5e6f7/attachments/AttOne",
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "upstream_unavailable"
    );
}

/// Header injection through a filename is not a thing.
#[test]
fn content_disposition_cannot_inject_a_header() {
    let nasty = "in\r\nX-Evil: yes\"\\.pdf";
    let value = content_disposition(nasty);
    assert!(!value.contains('\r') && !value.contains('\n'), "{value}");
    assert!(value.is_ascii(), "{value}");
    assert!(!value.contains("X-Evil: yes\""), "{value}");

    // Unprintable bytes become underscores rather than vanishing, so the
    // fallback filename is still a filename.
    assert!(
        content_disposition("\u{0}\u{1}").contains("filename=\"__\""),
        "{}",
        content_disposition("\u{0}\u{1}")
    );
    // A name with nothing usable left falls back to a literal.
    assert!(content_disposition("").contains("filename=\"attachment\""));
    assert!(content_disposition("   ").contains("filename=\"attachment\""));
}

// --------------------------------------------------------------- plans --

/// Criterion I7 - the thread list must walk `thread_labels_keyset_idx`.
#[tokio::test]
async fn thread_list_query_uses_an_index() {
    let app = test_app(Env::Prod).await;
    let account = connected(&app).await;

    // 20 000 threads across 4 labels.
    sqlx::query(
        "insert into threads (account_id, thread_id, last_ts, subject, snippet, \
                              from_name, from_email, unread, msg_count) \
         select $1, 't' || g, now() - (g || ' seconds')::interval, 's', 'p', 'n', 'e', \
                g % 3 = 0, 1 \
           from generate_series(1, 20000) g",
    )
    .bind(account)
    .execute(&app.db.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into thread_labels (account_id, thread_id, label_id, last_ts, unread) \
         select $1, t.thread_id, \
                (array['INBOX','SENT','Label_12','CATEGORY_UPDATES'])[1 + (g % 4)], \
                t.last_ts, t.unread \
           from generate_series(1, 20000) g \
           join threads t on t.thread_id = 't' || g and t.account_id = $1",
    )
    .bind(account)
    .execute(&app.db.pool)
    .await
    .unwrap();
    sqlx::query("analyze threads")
        .execute(&app.db.pool)
        .await
        .unwrap();
    sqlx::query("analyze thread_labels")
        .execute(&app.db.pool)
        .await
        .unwrap();

    let plan: Value = sqlx::query_scalar(
        "explain (format json) \
         select t.thread_id, t.subject, t.snippet, t.from_name, t.from_email, \
                t.last_ts, t.unread, t.msg_count \
           from thread_labels tl \
           join threads t \
             on t.account_id = tl.account_id and t.thread_id = tl.thread_id \
          where tl.account_id = $1 \
            and tl.label_id = $2 \
            and ($3::timestamptz is null \
                 or (tl.last_ts, tl.thread_id) < ($3::timestamptz, $4::text)) \
          order by tl.last_ts desc, tl.thread_id desc \
          limit $5",
    )
    .bind(account)
    .bind("INBOX")
    .bind(Option::<DateTime<Utc>>::None)
    .bind(Option::<String>::None)
    .bind(51i64)
    .fetch_one(&app.db.pool)
    .await
    .unwrap();

    let rendered = plan.to_string();
    println!("thread-list EXPLAIN: {rendered}");
    assert!(
        rendered.contains("thread_labels_keyset_idx"),
        "the thread list must walk thread_labels_keyset_idx: {rendered}"
    );
    assert!(
        !rendered.contains("Seq Scan"),
        "the thread list degraded to a sequential scan: {rendered}"
    );
    assert!(
        !rendered.contains("\"Sort\""),
        "the keyset index provides the order; a Sort node means it did not: {rendered}"
    );
}
