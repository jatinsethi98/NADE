//! `/drafts`: the list and the one edit path v1 has.

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

async fn a_draft(app: &TestApp, account: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "insert into drafts (id, account_id, thread_id, to_json, subject, body_text) \
         values ($1, $2, 't1', $3, 'Re: hi', 'original body')",
    )
    .bind(id)
    .bind(account)
    .bind(json!(["priya@kettle.com"]))
    .execute(&app.db.pool)
    .await
    .unwrap();
    id
}

async fn patch(
    app: &TestApp,
    token: &str,
    id: Uuid,
    body: &Value,
) -> axum::http::Response<axum::body::Body> {
    send(
        &app.router,
        "PATCH",
        &format!("/v1/drafts/{id}"),
        Some(token),
        Some(("application/json", body.to_string())),
    )
    .await
}

#[tokio::test]
async fn the_list_carries_the_whole_draft() {
    // Which is why there is deliberately no `GET /drafts/{id}` (`API.md` §11).
    let (app, token, account) = ctx().await;
    a_draft(&app, account).await;
    let body = response_json(get(&app, "/v1/drafts", Some(&token)).await).await;
    let row = &body["drafts"][0];
    assert_eq!(row["to"], json!(["priya@kettle.com"]));
    assert_eq!(row["subject"], json!("Re: hi"));
    assert_eq!(row["body_text"], json!("original body"));
    assert_eq!(row["thread_id"], json!("t1"));
}

#[tokio::test]
async fn there_is_no_get_for_a_single_draft() {
    let (app, token, account) = ctx().await;
    let id = a_draft(&app, account).await;
    // 405, not 404: the path exists for PATCH. Either way it is not a read.
    let status = get(&app, &format!("/v1/drafts/{id}"), Some(&token))
        .await
        .status();
    assert!(status == 405 || status == 404, "got {status}");
}

#[tokio::test]
async fn a_patch_returns_the_full_draft_and_changes_only_what_it_names() {
    let (app, token, account) = ctx().await;
    let id = a_draft(&app, account).await;

    let body = response_json(patch(&app, &token, id, &json!({"body_text": "edited"})).await).await;
    assert_eq!(body["body_text"], json!("edited"));
    assert_eq!(body["subject"], json!("Re: hi"), "an untouched field moved");
    assert_eq!(body["to"], json!(["priya@kettle.com"]));
    assert_eq!(body["id"], json!(id));
}

#[tokio::test]
async fn an_empty_patch_is_a_bad_request() {
    // `API.md` §3. A PATCH that changes nothing is a caller bug, and a 200
    // would hide it.
    let (app, token, account) = ctx().await;
    let id = a_draft(&app, account).await;
    assert_eq!(patch(&app, &token, id, &json!({})).await.status(), 400);
}

#[tokio::test]
async fn every_address_must_contain_an_at_sign() {
    let (app, token, account) = ctx().await;
    let id = a_draft(&app, account).await;
    for to in [
        json!(["everyone"]),
        json!([]),
        json!(["@x.com"]),
        json!(["a@"]),
    ] {
        let response = patch(&app, &token, id, &json!({"to": to})).await;
        assert_eq!(response.status(), 400, "{to}");
    }
}

#[tokio::test]
async fn a_valid_recipient_list_replaces_the_old_one() {
    let (app, token, account) = ctx().await;
    let id = a_draft(&app, account).await;
    let body =
        response_json(patch(&app, &token, id, &json!({"to": ["a@b.com", "c@d.com"]})).await).await;
    assert_eq!(body["to"], json!(["a@b.com", "c@d.com"]));
}

#[tokio::test]
async fn a_control_character_in_an_edit_is_stripped_before_it_is_stored() {
    // The same sanitising the tool does: a NUL here would be rejected by the
    // column, and an escape sequence has no business in a draft body.
    let (app, token, account) = ctx().await;
    let id = a_draft(&app, account).await;
    let hostile = format!("clean{}hidden", '\u{0000}');
    let body = response_json(patch(&app, &token, id, &json!({"body_text": hostile})).await).await;
    assert_eq!(body["body_text"], json!("cleanhidden"));
}

#[tokio::test]
async fn an_oversized_body_is_truncated_rather_than_refused() {
    let (app, token, account) = ctx().await;
    let id = a_draft(&app, account).await;
    let huge = "x".repeat(200 * 1024);
    let body = response_json(patch(&app, &token, id, &json!({"body_text": huge})).await).await;
    let stored = body["body_text"].as_str().unwrap();
    assert!(stored.len() <= 64 * 1024 + 64);
    assert!(stored.contains("truncated"));
}

#[tokio::test]
async fn an_empty_draft_list_is_an_empty_array_and_a_null_cursor() {
    let (app, token, _) = ctx().await;
    let body = response_json(get(&app, "/v1/drafts", Some(&token)).await).await;
    assert_eq!(body["drafts"], json!([]));
    assert_eq!(body["next_cursor"], Value::Null);
}

#[tokio::test]
async fn another_accounts_draft_cannot_be_read_or_edited() {
    let (app, token, _) = ctx().await;
    let other: Uuid =
        sqlx::query_scalar("insert into accounts (email) values ('b@x.com') returning id")
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
    let id = a_draft(&app, other).await;

    let list = response_json(get(&app, "/v1/drafts", Some(&token)).await).await;
    assert_eq!(list["drafts"], json!([]));
    assert_eq!(
        patch(&app, &token, id, &json!({"subject": "mine now"}))
            .await
            .status(),
        404
    );
}

#[tokio::test]
async fn the_draft_routes_are_behind_the_bearer_guard() {
    let (app, _, _) = ctx().await;
    assert_eq!(
        send(&app.router, "GET", "/v1/drafts", None, None)
            .await
            .status(),
        401
    );
    assert_eq!(
        send(
            &app.router,
            "PATCH",
            "/v1/drafts/00000000-0000-0000-0000-000000000000",
            None,
            Some(("application/json", "{}".to_owned()))
        )
        .await
        .status(),
        401
    );
}
