//! `/runs`: the Run log, and one run's journal served verbatim.

use serde_json::{json, Value};
use uuid::Uuid;

use crate::config::Env;
use crate::test_support::{get, response_json, send, test_app, TestApp};

/// Exactly one account, so `Auth`'s sole-account fallback resolves it.
async fn ctx() -> (TestApp, String, Uuid) {
    let app = test_app(Env::Dev).await;
    let token = app.device_token().await;
    let account: Uuid = sqlx::query_scalar("insert into accounts (email) values ($1) returning id")
        .bind("owner@example.com")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    (app, token, account)
}

async fn an_agent(app: &TestApp, account: Uuid) -> Uuid {
    sqlx::query_scalar(
        "insert into agents (account_id, name, nl_definition) values ($1, 'Tester', 'x') \
         returning id",
    )
    .bind(account)
    .fetch_one(&app.db.pool)
    .await
    .unwrap()
}

async fn a_run(app: &TestApp, account: Uuid, agent: Uuid, status: &str) -> Uuid {
    sqlx::query_scalar(
        "insert into agent_runs (agent_id, account_id, trigger_kind, status, summary) \
         values ($1, $2, 'mail', $3, 'Two next steps found') returning id",
    )
    .bind(agent)
    .bind(account)
    .bind(status)
    .fetch_one(&app.db.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn the_run_log_is_newest_first_and_names_its_agent() {
    let (app, token, account) = ctx().await;
    let agent = an_agent(&app, account).await;
    let older = a_run(&app, account, agent, "done").await;
    let newer = a_run(&app, account, agent, "failed").await;

    let body = response_json(get(&app, "/v1/runs", Some(&token)).await).await;
    let ids: Vec<&str> = body["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![newer.to_string(), older.to_string()]);
    assert_eq!(body["runs"][0]["agent_name"], json!("Tester"));
    assert_eq!(body["next_cursor"], Value::Null);
}

#[tokio::test]
async fn the_run_log_can_be_filtered_to_one_agent() {
    let (app, token, account) = ctx().await;
    let mine = an_agent(&app, account).await;
    let other = an_agent(&app, account).await;
    a_run(&app, account, mine, "done").await;
    a_run(&app, account, other, "done").await;

    let body =
        response_json(get(&app, &format!("/v1/runs?agent_id={mine}"), Some(&token)).await).await;
    assert_eq!(body["runs"].as_array().unwrap().len(), 1);
    assert_eq!(body["runs"][0]["agent_id"], json!(mine));
}

#[tokio::test]
async fn the_page_boundary_is_exact_and_the_cursor_walks_it_without_gaps() {
    // 51 rows against a page of 50: the 50th must not repeat on page two and
    // the 51st must not be skipped.
    let (app, token, account) = ctx().await;
    let agent = an_agent(&app, account).await;
    for _ in 0..51 {
        a_run(&app, account, agent, "done").await;
    }

    let first = response_json(get(&app, "/v1/runs", Some(&token)).await).await;
    assert_eq!(first["runs"].as_array().unwrap().len(), 50);
    let cursor = first["next_cursor"]
        .as_str()
        .expect("a next page")
        .to_owned();

    let second =
        response_json(get(&app, &format!("/v1/runs?cursor={cursor}"), Some(&token)).await).await;
    assert_eq!(second["runs"].as_array().unwrap().len(), 1);
    assert_eq!(
        second["next_cursor"],
        Value::Null,
        "the last page ends the walk"
    );

    let mut seen: Vec<&str> = first["runs"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["runs"].as_array().unwrap())
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    let total = seen.len();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), total, "a row was served twice");
    assert_eq!(total, 51, "a row was skipped");
}

#[tokio::test]
async fn exactly_one_page_of_rows_offers_no_next_cursor() {
    let (app, token, account) = ctx().await;
    let agent = an_agent(&app, account).await;
    for _ in 0..50 {
        a_run(&app, account, agent, "done").await;
    }
    let body = response_json(get(&app, "/v1/runs", Some(&token)).await).await;
    assert_eq!(body["runs"].as_array().unwrap().len(), 50);
    assert_eq!(body["next_cursor"], Value::Null);
}

#[tokio::test]
async fn an_empty_run_log_is_an_empty_array_and_a_null_cursor() {
    // `API.md` §0: never a 404.
    let (app, token, _) = ctx().await;
    let body = response_json(get(&app, "/v1/runs", Some(&token)).await).await;
    assert_eq!(body["runs"], json!([]));
    assert_eq!(body["next_cursor"], Value::Null);
}

#[tokio::test]
async fn a_corrupt_cursor_is_a_bad_request_and_not_a_silent_reset_to_page_one() {
    let (app, token, _) = ctx().await;
    for cursor in ["nonsense", "!!!", "eyJ0cyI6MX0"] {
        let response = get(&app, &format!("/v1/runs?cursor={cursor}"), Some(&token)).await;
        assert_eq!(response.status(), 400, "{cursor}");
    }
}

#[tokio::test]
async fn the_journal_is_served_verbatim() {
    // `API.md` §6.1: `run_journal` has exactly one author, the engine, and this
    // endpoint does not translate, reorder or summarise what it wrote.
    let (app, token, account) = ctx().await;
    let agent = an_agent(&app, account).await;
    let run = a_run(&app, account, agent, "done").await;

    let payload =
        json!({"turn": 1, "text": "hi", "usage": {"input_tokens": 5, "output_tokens": 1}});
    sqlx::query(
        "insert into run_journal (run_id, seq, kind, payload, created_at) \
         values ($1, 1, 'run_started', '{}'::jsonb, '2026-08-16T09:00:00Z'), \
                ($1, 2, 'model_response', $2, '2026-08-16T09:00:01Z')",
    )
    .bind(run)
    .bind(&payload)
    .execute(&app.db.pool)
    .await
    .unwrap();

    let body = response_json(get(&app, &format!("/v1/runs/{run}"), Some(&token)).await).await;
    let journal = body["journal"].as_array().unwrap();
    assert_eq!(journal.len(), 2);
    assert_eq!(journal[0]["seq"], json!(1));
    assert_eq!(journal[1]["kind"], json!("model_response"));
    assert_eq!(
        journal[1]["payload"], payload,
        "the payload was rewritten in transit"
    );
    // `API.md` §0: second precision, always Z-suffixed, never milliseconds.
    assert_eq!(journal[0]["created_at"], json!("2026-08-16T09:00:00Z"));
}

#[tokio::test]
async fn another_accounts_run_is_a_404() {
    let (app, token, account) = ctx().await;
    let agent = an_agent(&app, account).await;
    let run = a_run(&app, account, agent, "done").await;
    // Re-point the run at a second account; the first token must not see it.
    let other: Uuid =
        sqlx::query_scalar("insert into accounts (email) values ('b@x.com') returning id")
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
    sqlx::query("update agent_runs set account_id = $2 where id = $1")
        .bind(run)
        .bind(other)
        .execute(&app.db.pool)
        .await
        .unwrap();

    let response = get(&app, &format!("/v1/runs/{run}"), Some(&token)).await;
    assert_eq!(response.status(), 404);
}

#[tokio::test]
async fn an_unknown_run_is_a_404_and_a_malformed_id_is_a_400() {
    let (app, token, _) = ctx().await;
    assert_eq!(
        get(&app, &format!("/v1/runs/{}", Uuid::new_v4()), Some(&token))
            .await
            .status(),
        404
    );
    assert_eq!(
        get(&app, "/v1/runs/not-a-uuid", Some(&token))
            .await
            .status(),
        400
    );
}

#[tokio::test]
async fn the_run_routes_are_behind_the_bearer_guard() {
    let (app, _, _) = ctx().await;
    for path in ["/v1/runs", "/v1/runs/00000000-0000-0000-0000-000000000000"] {
        assert_eq!(
            send(&app.router, "GET", path, None, None).await.status(),
            401
        );
    }
}

#[tokio::test]
async fn a_cursor_from_another_endpoint_is_a_400_and_not_a_silent_skip() {
    // `cursor::Payload.id` is a plain string because `/mailboxes/{id}/threads`
    // puts a **Gmail thread id** there, so a well-formed cursor from that
    // endpoint decodes here without error. Parsed with
    // `unwrap_or(Uuid::nil())` - the obvious spelling - it became the smallest
    // UUID and silently skipped every row inside the cursor's own second.
    // `API.md` §0 forbids even the benign version: "never a silent reset to
    // page one".
    let (app, token, account) = ctx().await;
    let agent = an_agent(&app, account).await;
    a_run(&app, account, agent, "done").await;

    let foreign = crate::api::cursor::encode(chrono::Utc::now(), "18f2a1b3c4d5e6f7");
    for path in [
        format!("/v1/runs?cursor={foreign}"),
        format!("/v1/notes?cursor={foreign}"),
        format!("/v1/drafts?cursor={foreign}"),
    ] {
        let response = get(&app, &path, Some(&token)).await;
        assert_eq!(response.status(), 400, "{path}");
        assert_eq!(
            response_json(response).await["error"]["code"],
            json!("bad_request")
        );
    }
}

#[tokio::test]
async fn rows_sharing_a_timestamp_paginate_without_skipping_or_repeating() {
    // Every existing pagination test inserted rows one statement at a time, so
    // each got a distinct microsecond and the `(ts, id)` tuple was never the
    // thing under test. The tie is the realistic case: P5's mail trigger will
    // create several runs inside one transaction, where `now()` is identical
    // for all of them.
    let (app, token, account) = ctx().await;
    let agent = an_agent(&app, account).await;

    sqlx::query(
        "insert into agent_runs (agent_id, account_id, trigger_kind, status, created_at) \
         select $1, $2, 'mail', 'done', timestamptz '2026-08-16 09:00:00Z' \
           from generate_series(1, 51)",
    )
    .bind(agent)
    .bind(account)
    .execute(&app.db.pool)
    .await
    .unwrap();

    let mut seen: Vec<String> = Vec::new();
    let mut url = "/v1/runs".to_owned();
    for _ in 0..5 {
        let body = response_json(get(&app, &url, Some(&token)).await).await;
        for row in body["runs"].as_array().unwrap() {
            seen.push(row["id"].as_str().unwrap().to_owned());
        }
        match body["next_cursor"].as_str() {
            None => break,
            Some(cursor) => url = format!("/v1/runs?cursor={cursor}"),
        }
    }

    let total = seen.len();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), total, "a row was served twice");
    assert_eq!(total, 51, "a row was skipped: {total} of 51");
}
