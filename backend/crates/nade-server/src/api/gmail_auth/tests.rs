//! OAuth route tests. The consent click itself needs a human, so everything
//! here drives the two routes against a local Google.

use axum::http::StatusCode;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

use crate::{
    config::Env,
    test_support::{get as authed_get, response_json, test_app, TestApp},
};

/// A client file that points at a local "Google".
fn client_file(server: &MockServer) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("nade-oauth-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(
        &path,
        format!(
            r#"{{"web":{{"client_id":"test-client-id","client_secret":"test-secret",
                "auth_uri":"{base}/auth","token_uri":"{base}/token"}}}}"#,
            base = server.uri()
        ),
    )
    .unwrap();
    path
}

async fn app_with_oauth(server: &MockServer) -> TestApp {
    let mut app = test_app(Env::Prod).await;
    app.set_gmail_client_file(client_file(server));
    app.set_gmail_base(&server.uri());
    app
}

async fn body_text(response: axum::http::Response<axum::body::Body>) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Drive `start`, then hand the `state` it minted to `callback`.
async fn start_and_state(app: &TestApp) -> String {
    let response = authed_get(app, "/v1/auth/gmail/start", None).await;
    assert_eq!(response.status(), StatusCode::FOUND);
    let location = response
        .headers()
        .get(axum::http::header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    url::form_urlencoded_state(&location)
}

/// Tiny query-string reader, so the test does not depend on a URL crate.
mod url {
    pub fn form_urlencoded_state(location: &str) -> String {
        let query = location.split_once('?').map(|(_, q)| q).unwrap_or_default();
        for pair in query.split('&') {
            if let Some(value) = pair.strip_prefix("state=") {
                return percent_encoding::percent_decode_str(value)
                    .decode_utf8_lossy()
                    .into_owned();
            }
        }
        panic!("the consent URL carried no state: {location}");
    }
}

fn google_token(server: &MockServer) -> Mock {
    Mock::given(method("POST")).and(path("/token")).respond_with(
        ResponseTemplate::new(200).set_body_raw(
            br#"{"access_token":"ya29.fresh","token_type":"Bearer","expires_in":3599,
                 "refresh_token":"1//0gRefresh","scope":"https://www.googleapis.com/auth/gmail.readonly"}"#
                .to_vec(),
            "application/json",
        ),
    )
    .expect(1..)
    .named(format!("token endpoint on {}", server.uri()))
}

fn google_profile(email: &str) -> Mock {
    let body =
        format!(r#"{{"emailAddress":"{email}","messagesTotal":63120,"historyId":"9412771"}}"#);
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/profile"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(body.into_bytes(), "application/json"),
        )
}

/// Criterion K1.
#[tokio::test]
async fn start_redirects_with_pkce_and_state() {
    let server = MockServer::start().await;
    let app = app_with_oauth(&server).await;

    let response = authed_get(&app, "/v1/auth/gmail/start", None).await;
    assert_eq!(response.status(), StatusCode::FOUND);
    let location = response
        .headers()
        .get(axum::http::header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();

    for fragment in [
        "code_challenge=",
        "code_challenge_method=S256",
        "state=",
        "access_type=offline",
        "prompt=consent",
        "response_type=code",
        "gmail.readonly",
    ] {
        assert!(
            location.contains(fragment),
            "{fragment} missing: {location}"
        );
    }
    assert!(
        !location.contains("test-secret"),
        "the client secret must never reach the browser: {location}"
    );
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CACHE_CONTROL)
            .unwrap(),
        "no-store"
    );
    assert_eq!(app.state.gmail.pending.len(), 1);
}

/// Criterion O12 - both OAuth routes are reachable without a bearer, and
/// nothing else new is.
#[tokio::test]
async fn the_oauth_routes_are_public_and_the_rest_are_not() {
    let server = MockServer::start().await;
    let app = app_with_oauth(&server).await;

    for path in ["/v1/auth/gmail/start", "/v1/auth/gmail/callback"] {
        let status = authed_get(&app, path, None).await.status();
        assert_ne!(
            status,
            StatusCode::UNAUTHORIZED,
            "{path} is browser-facing and cannot carry a bearer token"
        );
    }

    for path in [
        "/v1/me",
        "/v1/mailboxes",
        "/v1/mailboxes/INBOX/threads",
        "/v1/threads/abc",
        "/v1/search?q=x",
        "/v1/messages/abc/attachments/def",
    ] {
        let response = authed_get(&app, path, None).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{path} must be behind the bearer guard"
        );
        assert_eq!(
            response_json(response).await["error"]["code"],
            "unauthorized"
        );
    }
}

/// Criterion K3.
#[tokio::test]
async fn callback_binds_the_account_and_renders_a_close_page() {
    let server = MockServer::start().await;
    google_token(&server).mount(&server).await;
    google_profile("jatinsethi98@gmail.com")
        .mount(&server)
        .await;
    let app = app_with_oauth(&server).await;

    let state = start_and_state(&app).await;
    let response = authed_get(
        &app,
        &format!("/v1/auth/gmail/callback?state={state}&code=4/0Aauth-code&scope=x"),
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap(),
        "text/html; charset=utf-8"
    );
    let html = body_text(response).await;
    assert!(html.contains("close this tab"), "{html}");
    assert!(html.contains("jatinsethi98@gmail.com"), "{html}");

    // The account is bound, with tokens stored as ciphertext.
    let (email, status): (String, String) = sqlx::query_as("select email, status from accounts")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(email, "jatinsethi98@gmail.com");
    assert_eq!(status, "ok");

    let (access, refresh): (String, String) =
        sqlx::query_as("select access_token, refresh_token from gmail_tokens")
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
    assert!(!access.contains("ya29.fresh"), "{access}");
    assert!(!refresh.contains("1//0gRefresh"), "{refresh}");
    let cipher = app.gmail_cipher();
    assert_eq!(cipher.decrypt(&access).unwrap(), "ya29.fresh");
    assert_eq!(cipher.decrypt(&refresh).unwrap(), "1//0gRefresh");

    // A first sync is queued, so mail starts landing without another click.
    let queued: i64 = sqlx::query_scalar("select count(*) from jobs where kind = 'gmail_sync'")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(queued, 1);

    // And it is audited.
    let audited: i64 =
        sqlx::query_scalar("select count(*) from audit_log where action = 'gmail_consent'")
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
    assert_eq!(audited, 1);
}

/// Criterion K2.
#[tokio::test]
async fn callback_rejects_an_unknown_or_replayed_state() {
    let server = MockServer::start().await;
    google_token(&server).mount(&server).await;
    google_profile("jatinsethi98@gmail.com")
        .mount(&server)
        .await;
    let app = app_with_oauth(&server).await;

    // Never issued.
    let response = authed_get(
        &app,
        "/v1/auth/gmail/callback?state=made-up&code=4/0Aauth-code",
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(body_text(response).await.contains("expired"));

    // Issued, used once...
    let state = start_and_state(&app).await;
    let first = authed_get(
        &app,
        &format!("/v1/auth/gmail/callback?state={state}&code=4/0Aauth-code"),
        None,
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);

    // ...and replayed. A stolen callback URL is worth nothing.
    let replay = authed_get(
        &app,
        &format!("/v1/auth/gmail/callback?state={state}&code=4/0Aauth-code"),
        None,
    )
    .await;
    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);

    let accounts: i64 = sqlx::query_scalar("select count(*) from accounts")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(accounts, 1, "a replay must not create a second account");
}

#[tokio::test]
async fn callback_reports_a_refusal_or_a_missing_code() {
    let server = MockServer::start().await;
    let app = app_with_oauth(&server).await;

    // The user pressed Cancel on the consent screen.
    let response = authed_get(
        &app,
        "/v1/auth/gmail/callback?error=access_denied&state=x",
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // EDGE (empty input).
    for query in ["", "?", "?state=x", "?code=y", "?state=&code="] {
        let response = authed_get(&app, &format!("/v1/auth/gmail/callback{query}"), None).await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "callback{query:?} must not be accepted"
        );
    }
}

/// v1 is single-account. A second mailbox would repoint every stored message.
#[tokio::test]
async fn a_second_google_account_is_refused() {
    let server = MockServer::start().await;
    google_token(&server).mount(&server).await;
    google_profile("someone.else@gmail.com")
        .mount(&server)
        .await;
    let app = app_with_oauth(&server).await;

    sqlx::query("insert into accounts (email) values ('jatinsethi98@gmail.com')")
        .execute(&app.db.pool)
        .await
        .unwrap();

    let state = start_and_state(&app).await;
    let response = authed_get(
        &app,
        &format!("/v1/auth/gmail/callback?state={state}&code=4/0Aauth-code"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(body_text(response).await.contains("already connected"));

    let emails: Vec<String> = sqlx::query_scalar("select email from accounts")
        .fetch_all(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(emails, vec!["jatinsethi98@gmail.com"]);
}

/// A server without `secrets/web_client.json` must still boot and say so.
#[tokio::test]
async fn an_unconfigured_server_explains_itself() {
    let app = test_app(Env::Prod).await;
    assert!(app.state.gmail.oauth.is_none());

    let response = authed_get(&app, "/v1/auth/gmail/start", None).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(body_text(response).await.contains("web_client.json"));

    // And every other route still works.
    let token = app.device_token().await;
    assert_eq!(
        authed_get(&app, "/v1/me", Some(&token)).await.status(),
        StatusCode::OK
    );
}

/// Google refusing the code is a readable page, not a stack trace.
#[tokio::test]
async fn a_failed_exchange_is_a_readable_page() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_raw(
            br#"{"error":"invalid_grant","error_description":"Bad Request"}"#.to_vec(),
            "application/json",
        ))
        .mount(&server)
        .await;
    let app = app_with_oauth(&server).await;

    let state = start_and_state(&app).await;
    let response = authed_get(
        &app,
        &format!("/v1/auth/gmail/callback?state={state}&code=stale"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let html = body_text(response).await;
    assert!(html.contains("refused the sign-in"), "{html}");
    assert!(
        !html.contains("invalid_grant"),
        "no upstream detail: {html}"
    );
}
