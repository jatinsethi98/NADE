//! `/notes`: the list, the search, and the read that marks it read.

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

async fn a_note(app: &TestApp, account: Uuid, title: &str, body: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("insert into notes (id, account_id, title, body_md) values ($1, $2, $3, $4)")
        .bind(id)
        .bind(account)
        .bind(title)
        .bind(body)
        .execute(&app.db.pool)
        .await
        .unwrap();
    id
}

#[tokio::test]
async fn a_note_with_no_run_behind_it_has_no_agent_name() {
    // `API.md` §3: "`agent_name` is null for a note with no run behind it."
    let (app, token, account) = ctx().await;
    a_note(&app, account, "Welcome", "hello").await;
    let body = response_json(get(&app, "/v1/notes", Some(&token)).await).await;
    assert_eq!(body["notes"][0]["agent_name"], Value::Null);
    assert_eq!(body["notes"][0]["run_id"], Value::Null);
    assert_eq!(body["notes"][0]["unread"], json!(true));
}

#[tokio::test]
async fn reading_a_note_marks_it_read_and_the_response_reports_the_state_after() {
    let (app, token, account) = ctx().await;
    let id = a_note(&app, account, "Kettle", "# next steps").await;

    let body = response_json(get(&app, &format!("/v1/notes/{id}"), Some(&token)).await).await;
    // `API.md` §3: "Reading a note marks it read. The response therefore always
    // shows `unread: false` - it reports the state *after* the read."
    assert_eq!(body["unread"], json!(false));
    assert_eq!(body["body_md"], json!("# next steps"));

    // And the list agrees afterwards.
    let list = response_json(get(&app, "/v1/notes", Some(&token)).await).await;
    assert_eq!(list["notes"][0]["unread"], json!(false));
}

#[tokio::test]
async fn reading_a_note_twice_is_not_an_error() {
    let (app, token, account) = ctx().await;
    let id = a_note(&app, account, "t", "b").await;
    for _ in 0..2 {
        let response = get(&app, &format!("/v1/notes/{id}"), Some(&token)).await;
        assert_eq!(response.status(), 200);
        assert_eq!(response_json(response).await["unread"], json!(false));
    }
}

#[tokio::test]
async fn opening_a_note_does_not_move_it_to_the_top_of_the_list() {
    // The list is ordered by `updated_at`; reading is not an edit, so a note
    // must not jump the queue just because it was opened.
    let (app, token, account) = ctx().await;
    let older = a_note(&app, account, "older", "b").await;
    let _newer = a_note(&app, account, "newer", "b").await;

    let before = response_json(get(&app, "/v1/notes", Some(&token)).await).await;
    let order_before: Vec<&str> = before["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["title"].as_str().unwrap())
        .collect();

    get(&app, &format!("/v1/notes/{older}"), Some(&token)).await;

    let after = response_json(get(&app, "/v1/notes", Some(&token)).await).await;
    let order_after: Vec<&str> = after["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["title"].as_str().unwrap())
        .collect();
    assert_eq!(order_before, order_after);
}

#[tokio::test]
async fn the_search_is_a_case_insensitive_substring_over_title_and_body() {
    let (app, token, account) = ctx().await;
    a_note(&app, account, "Kettle interview", "nothing here").await;
    a_note(&app, account, "Groceries", "buy a KETTLE").await;
    a_note(&app, account, "Unrelated", "nothing").await;

    let body = response_json(get(&app, "/v1/notes?q=kettle", Some(&token)).await).await;
    assert_eq!(body["notes"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn a_wildcard_in_the_query_is_matched_literally() {
    // Built by the database, not by concatenation here, so `%` cannot act as a
    // wildcard a caller never asked for.
    let (app, token, account) = ctx().await;
    a_note(&app, account, "one hundred %", "b").await;
    a_note(&app, account, "unrelated", "b").await;

    let body = response_json(get(&app, "/v1/notes?q=%25", Some(&token)).await).await;
    assert_eq!(body["notes"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_unicode_query_matches() {
    // EDGE: unicode.
    let (app, token, account) = ctx().await;
    a_note(&app, account, "\u{4f60}\u{597d}", "b").await;
    let body = response_json(get(&app, "/v1/notes?q=%E4%BD%A0", Some(&token)).await).await;
    assert_eq!(body["notes"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn an_empty_note_list_is_an_empty_array_and_a_null_cursor() {
    let (app, token, _) = ctx().await;
    let body = response_json(get(&app, "/v1/notes", Some(&token)).await).await;
    assert_eq!(body["notes"], json!([]));
    assert_eq!(body["next_cursor"], Value::Null);
}

#[tokio::test]
async fn the_note_page_boundary_is_exact() {
    let (app, token, account) = ctx().await;
    for index in 0..51 {
        a_note(&app, account, &format!("n{index}"), "b").await;
    }
    let first = response_json(get(&app, "/v1/notes", Some(&token)).await).await;
    assert_eq!(first["notes"].as_array().unwrap().len(), 50);
    let cursor = first["next_cursor"]
        .as_str()
        .expect("a next page")
        .to_owned();
    let second =
        response_json(get(&app, &format!("/v1/notes?cursor={cursor}"), Some(&token)).await).await;
    assert_eq!(second["notes"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn another_accounts_note_is_a_404() {
    let (app, token, _) = ctx().await;
    let other: Uuid =
        sqlx::query_scalar("insert into accounts (email) values ('b@x.com') returning id")
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
    let id = a_note(&app, other, "theirs", "b").await;
    assert_eq!(
        get(&app, &format!("/v1/notes/{id}"), Some(&token))
            .await
            .status(),
        404
    );
}

#[tokio::test]
async fn the_note_routes_are_behind_the_bearer_guard() {
    let (app, _, _) = ctx().await;
    for path in [
        "/v1/notes",
        "/v1/notes/00000000-0000-0000-0000-000000000000",
    ] {
        assert_eq!(
            send(&app.router, "GET", path, None, None).await.status(),
            401
        );
    }
}

/// `GET /notes/{id}` joins the agent through the run, the same way the list
/// does. The list had a test for this and the detail did not — which is how the
/// detail path came to answer it with a second round trip nobody was watching.
#[tokio::test]
async fn reading_a_note_carries_the_agents_name_and_still_marks_it_read() {
    let (app, token, account) = ctx().await;
    let agent: Uuid = sqlx::query_scalar(
        "insert into agents (account_id, name, nl_definition, status) \
         values ($1, 'Job Search Tracker', 'when mail arrives, note it', 'published') \
         returning id",
    )
    .bind(account)
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    let run: Uuid = sqlx::query_scalar(
        "insert into agent_runs (agent_id, account_id, trigger_kind, status) \
         values ($1, $2, 'manual', 'done') returning id",
    )
    .bind(agent)
    .bind(account)
    .fetch_one(&app.db.pool)
    .await
    .unwrap();

    let id = Uuid::new_v4();
    sqlx::query(
        "insert into notes (id, account_id, run_id, title, body_md) values ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(account)
    .bind(run)
    .bind("Next steps")
    .bind("- reply")
    .execute(&app.db.pool)
    .await
    .unwrap();

    let body = response_json(get(&app, &format!("/v1/notes/{id}"), Some(&token)).await).await;
    assert_eq!(body["agent_name"], json!("Job Search Tracker"));
    assert_eq!(body["run_id"], json!(run.to_string()));
    assert_eq!(body["unread"], json!(false));

    // The detail and the list must agree about the name, since they now answer
    // it with the same join.
    let list = response_json(get(&app, "/v1/notes", Some(&token)).await).await;
    assert_eq!(list["notes"][0]["agent_name"], json!("Job Search Tracker"));
    assert_eq!(list["notes"][0]["unread"], json!(false));
}

/// EDGE (duplicate delivery): two reads of the same note answer identically and
/// neither is an edit. `opening_a_note_does_not_move_it_to_the_top_of_the_list`
/// asserts the consequence through the list's order; this asserts the column
/// directly, on the path the `update ... returning` CTE rewrote.
#[tokio::test]
async fn reading_a_note_twice_is_idempotent_and_does_not_touch_updated_at() {
    let (app, token, account) = ctx().await;
    let id = a_note(&app, account, "Kettle", "body").await;
    let before: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("select updated_at from notes where id = $1")
            .bind(id)
            .fetch_one(&app.db.pool)
            .await
            .unwrap();

    for _ in 0..2 {
        let body = response_json(get(&app, &format!("/v1/notes/{id}"), Some(&token)).await).await;
        assert_eq!(body["unread"], json!(false));
        assert_eq!(body["agent_name"], Value::Null);
    }

    let after: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("select updated_at from notes where id = $1")
            .bind(id)
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
    assert_eq!(before, after, "opening a note is not an edit");
}
