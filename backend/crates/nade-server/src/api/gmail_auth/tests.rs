//! OAuth route tests. The consent click itself needs a human, so everything
//! here drives the three routes against a local Google.
//!
//! The shape of a whole run is: pair a device → `POST /v1/auth/gmail/link` for a
//! capability → `GET /v1/auth/gmail/start?cap=…` for the redirect **and** the
//! binding cookie → `GET /v1/auth/gmail/callback?state=…&code=…` carrying that
//! cookie. Every one of those four steps is a gate, and each has its own test
//! for what happens when it is skipped.

use std::time::{Duration, Instant};

use axum::{
    body::Body,
    http::{header, Request, Response, StatusCode},
};
use tower::ServiceExt as _;
use uuid::Uuid;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

use super::{binding_cookie, cookie_path, start_url, BINDING_COOKIE};
use crate::{
    config::Env,
    gmail::oauth::{Pending, ACCOUNT_SINGLETON_LOCK},
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

async fn body_text(response: Response<Body>) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

// ------------------------------------------------------------- the plumbing --

/// A GET carrying an arbitrary set of headers - the only way to exercise the
/// binding cookie, which `test_support::get` cannot express.
async fn get_with(app: &TestApp, path: &str, headers: &[(&str, &[u8])]) -> Response<Body> {
    let mut builder = Request::builder().method("GET").uri(path);
    for (name, value) in headers {
        builder = builder.header(*name, axum::http::HeaderValue::from_bytes(value).unwrap());
    }
    app.router
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn post_authed(app: &TestApp, path: &str, token: Option<&str>) -> Response<Body> {
    let mut builder = Request::builder().method("POST").uri(path);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    app.router
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

/// One completed leg of the browser flow: the `state` Google will echo back, and
/// the cookie the browser now holds.
#[derive(Debug, Clone)]
struct Started {
    state: String,
    cookie: String,
}

fn header_value(response: &Response<Body>, name: header::HeaderName) -> String {
    response
        .headers()
        .get(name)
        .expect("header must be present")
        .to_str()
        .unwrap()
        .to_owned()
}

/// Mint a capability with a real paired device, exactly as the iOS app would.
async fn capability(app: &TestApp) -> String {
    let token = app.device_token().await;
    capability_for(app, &token).await
}

async fn capability_for(app: &TestApp, token: &str) -> String {
    let response = post_authed(app, "/v1/auth/gmail/link", Some(token)).await;
    assert_eq!(response.status(), StatusCode::OK, "link must succeed");
    let url = response_json(response).await["url"]
        .as_str()
        .expect("link returns a url")
        .to_owned();
    url.split_once("?cap=")
        .expect("the start URL carries the capability")
        .1
        .to_owned()
}

/// Drive `start` with a fresh capability and keep both halves of what it hands
/// the browser.
async fn start_flow(app: &TestApp) -> Started {
    let cap = capability(app).await;
    start_flow_with(app, &cap).await
}

async fn start_flow_with(app: &TestApp, cap: &str) -> Started {
    let response = authed_get(app, &format!("/v1/auth/gmail/start?cap={cap}"), None).await;
    assert_eq!(response.status(), StatusCode::FOUND, "start must redirect");
    let location = header_value(&response, header::LOCATION);
    let set_cookie = header_value(&response, header::SET_COOKIE);
    Started {
        state: url::form_urlencoded_state(&location),
        cookie: cookie_value(&set_cookie),
    }
}

/// `name=value; Attr; Attr` → the `name=value` a browser would send back.
fn cookie_value(set_cookie: &str) -> String {
    set_cookie
        .split(';')
        .next()
        .expect("a Set-Cookie always has a name=value")
        .to_owned()
}

/// Just the secret half of `name=value`.
fn cookie_secret(set_cookie: &str) -> String {
    cookie_value(set_cookie)
        .split_once('=')
        .expect("a cookie is name=value")
        .1
        .to_owned()
}

/// The callback leg, carrying the cookie the browser was given - which is what
/// the whole of F1's second half is about.
async fn complete(app: &TestApp, started: &Started) -> Response<Body> {
    get_with(
        app,
        &format!(
            "/v1/auth/gmail/callback?state={}&code=4/0Aauth-code&scope=x",
            started.state
        ),
        &[(header::COOKIE.as_str(), started.cookie.as_bytes())],
    )
    .await
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

// --------------------------------------------------------------- minting --

/// F1 - the whole defect in one assertion: the route that authorises linking is
/// behind the bearer guard.
#[tokio::test]
async fn link_requires_a_bearer_token() {
    let server = MockServer::start().await;
    let app = app_with_oauth(&server).await;

    for token in [None, Some(""), Some("nade_not-a-real-token")] {
        let response = post_authed(&app, "/v1/auth/gmail/link", token).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{token:?} must not mint a capability"
        );
        assert_eq!(
            response_json(response).await["error"]["code"],
            "unauthorized"
        );
    }
    assert!(
        app.state.gmail_link.is_empty(),
        "an unauthenticated caller must not be able to mint anything"
    );
}

#[tokio::test]
async fn link_returns_a_start_url_and_audits_the_request() {
    let server = MockServer::start().await;
    let app = app_with_oauth(&server).await;
    let token = app.device_token().await;

    let response = post_authed(&app, "/v1/auth/gmail/link", Some(&token)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;

    let url = body["url"].as_str().unwrap();
    assert!(
        url.starts_with("http://localhost:8080/v1/auth/gmail/start?cap="),
        "the URL is built from the registered redirect URI: {url}"
    );
    let cap = url.split_once("?cap=").unwrap().1;
    assert_eq!(cap.len(), 64, "32 bytes of randomness: {cap}");
    assert!(
        cap.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "the capability must need no percent-encoding: {cap}"
    );

    // API.md §0: ISO-8601 UTC, second precision, `Z`-suffixed.
    let expires_at = body["expires_at"].as_str().unwrap();
    assert_eq!(expires_at.len(), 20, "{expires_at}");
    assert!(expires_at.ends_with('Z'), "{expires_at}");
    let parsed = chrono::DateTime::parse_from_rfc3339(expires_at).expect("a real timestamp");
    let seconds = (parsed.to_utc() - chrono::Utc::now()).num_seconds();
    assert!(
        (595..=600).contains(&seconds),
        "ten minutes, got {seconds}s"
    );

    // Two calls never collide, and the paper trail names the device.
    let second = capability_for(&app, &token).await;
    assert_ne!(cap, second);

    let (actor, action): (String, String) = sqlx::query_as(
        "select actor, action from audit_log where action = 'gmail_link_requested' \
          order by id limit 1",
    )
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    assert!(actor.starts_with("device:"), "{actor}");
    assert_eq!(action, "gmail_link_requested");
}

// ----------------------------------------------------------------- start --

/// Criterion K1.
#[tokio::test]
async fn start_redirects_with_pkce_and_state() {
    let server = MockServer::start().await;
    let app = app_with_oauth(&server).await;

    let cap = capability(&app).await;
    let response = authed_get(&app, &format!("/v1/auth/gmail/start?cap={cap}"), None).await;
    assert_eq!(response.status(), StatusCode::FOUND);
    let location = header_value(&response, header::LOCATION);

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
    assert_eq!(header_value(&response, header::CACHE_CONTROL), "no-store");
    assert_eq!(app.state.gmail.pending.len(), 1);

    // The browser is bound as well as the flow.
    let set_cookie = header_value(&response, header::SET_COOKIE);
    assert!(
        set_cookie.starts_with(&format!("{BINDING_COOKIE}=")),
        "{set_cookie}"
    );
    assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
    assert!(set_cookie.contains("SameSite=Lax"), "{set_cookie}");
    assert!(set_cookie.contains("Max-Age=600"), "{set_cookie}");
    assert!(set_cookie.contains("Path=/v1/auth/gmail"), "{set_cookie}");
    // Dev is plain http, where a `Secure` cookie would simply be dropped.
    assert!(!set_cookie.contains("Secure"), "{set_cookie}");
    assert!(
        !location.contains(&cookie_secret(&set_cookie)),
        "the binding value must never leave in a URL: {location}"
    );
}

/// F1 - the core refusal. Every way of arriving without a live capability lands
/// on the same page, and none of them starts a flow.
#[tokio::test]
async fn start_without_a_capability_refuses() {
    let server = MockServer::start().await;
    let app = app_with_oauth(&server).await;

    for query in [
        "",
        "?",
        "?cap=",
        "?cap=nope",
        "?capability=x",
        // EDGE (unicode) and EDGE (oversized input).
        "?cap=%F0%9F%94%90",
        "?cap=../../etc/passwd",
        &format!("?cap={}", "a".repeat(4_096)),
    ] {
        let response = authed_get(&app, &format!("/v1/auth/gmail/start{query}"), None).await;
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "start{query:?} must be refused"
        );
        assert!(
            response.headers().get(header::LOCATION).is_none(),
            "a refusal is never a redirect: start{query:?}"
        );
        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "a refusal binds no browser: start{query:?}"
        );
        assert!(
            body_text(response)
                .await
                .contains("Start from the NADE app"),
            "start{query:?}"
        );
    }
    assert!(
        app.state.gmail.pending.is_empty(),
        "no refusal may leave a pending consent behind"
    );
}

/// EDGE (replay): the capability is single-use at the route, not only in the
/// store.
#[tokio::test]
async fn a_capability_is_single_use_at_the_route() {
    let server = MockServer::start().await;
    let app = app_with_oauth(&server).await;

    let cap = capability(&app).await;
    assert_eq!(
        authed_get(&app, &format!("/v1/auth/gmail/start?cap={cap}"), None)
            .await
            .status(),
        StatusCode::FOUND
    );
    let replay = authed_get(&app, &format!("/v1/auth/gmail/start?cap={cap}"), None).await;
    assert_eq!(replay.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        app.state.gmail.pending.len(),
        1,
        "the replay must not open a second consent"
    );
}

/// EDGE (expiry): ten minutes, planted rather than waited out.
#[tokio::test]
async fn an_expired_capability_is_refused() {
    let server = MockServer::start().await;
    let app = app_with_oauth(&server).await;

    let Some(long_ago) = Instant::now().checked_sub(Duration::from_secs(601)) else {
        println!("skipped: the monotonic clock has not been running for ten minutes");
        return;
    };
    let stale = "f".repeat(64);
    app.state
        .gmail_link
        .remember_at(stale.clone(), None, long_ago);

    let response = authed_get(&app, &format!("/v1/auth/gmail/start?cap={stale}"), None).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(app.state.gmail.pending.is_empty());
}

/// EDGE (valid capability, revoked device): revoking a device revokes what it
/// authorised.
#[tokio::test]
async fn a_capability_from_a_revoked_device_is_refused() {
    let server = MockServer::start().await;
    let app = app_with_oauth(&server).await;

    let token = app.device_token().await;
    let cap = capability_for(&app, &token).await;

    sqlx::query("update devices set revoked_at = now()")
        .execute(&app.db.pool)
        .await
        .unwrap();

    let response = authed_get(&app, &format!("/v1/auth/gmail/start?cap={cap}"), None).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(
        app.state.gmail.pending.is_empty(),
        "a revoked device opens no consent"
    );

    // And a capability whose device row was deleted outright is refused too.
    let orphan = "e".repeat(64);
    app.state
        .gmail_link
        .remember_at(orphan.clone(), Some(Uuid::new_v4()), Instant::now());
    assert_eq!(
        authed_get(&app, &format!("/v1/auth/gmail/start?cap={orphan}"), None)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
}

/// F1 - the refusal must not double as a "is this server up for grabs?" probe.
#[tokio::test]
async fn the_refusal_page_does_not_reveal_whether_a_mailbox_is_connected() {
    let server = MockServer::start().await;
    let app = app_with_oauth(&server).await;

    let fresh = authed_get(&app, "/v1/auth/gmail/start", None).await;
    let fresh_status = fresh.status();
    let fresh_body = body_text(fresh).await;

    sqlx::query("insert into accounts (email) values ('jatinsethi98@gmail.com')")
        .execute(&app.db.pool)
        .await
        .unwrap();

    let connected = authed_get(&app, "/v1/auth/gmail/start", None).await;
    assert_eq!(connected.status(), fresh_status);
    assert_eq!(
        body_text(connected).await,
        fresh_body,
        "the refusal must be byte-identical connected and not"
    );
    for leak in ["jatinsethi98", "already", "connected", "gmail.com"] {
        assert!(
            !fresh_body.to_lowercase().contains(leak),
            "the refusal leaked {leak:?}: {fresh_body}"
        );
    }
}

/// Criterion O12 - the browser routes still need no bearer header, and
/// everything else still does.
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

    // ...including the route that authorises them.
    assert_eq!(
        post_authed(&app, "/v1/auth/gmail/link", None)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );

    for path in [
        "/v1/me",
        "/v1/mailboxes",
        "/v1/mailboxes/INBOX/threads",
        "/v1/threads/abc",
        "/v1/search?q=x",
        "/v1/messages/abc/attachments/def",
        "/v1/auth/gmail/link",
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

// -------------------------------------------------------------- callback --

/// Criterion K3.
#[tokio::test]
async fn callback_binds_the_account_and_renders_a_close_page() {
    let server = MockServer::start().await;
    google_token(&server).mount(&server).await;
    google_profile("jatinsethi98@gmail.com")
        .mount(&server)
        .await;
    let app = app_with_oauth(&server).await;

    let started = start_flow(&app).await;
    let response = complete(&app, &started).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_value(&response, header::CONTENT_TYPE),
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
    let started = start_flow(&app).await;
    assert_eq!(complete(&app, &started).await.status(), StatusCode::OK);

    // ...and replayed. A stolen callback URL is worth nothing even with the
    // cookie, because the `state` is spent.
    assert_eq!(
        complete(&app, &started).await.status(),
        StatusCode::BAD_REQUEST
    );

    let accounts: i64 = sqlx::query_scalar("select count(*) from accounts")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(accounts, 1, "a replay must not create a second account");
}

/// F1 - the browser binding. A `state` lifted out of the redirect is useless
/// without the `HttpOnly` cookie that went with it.
#[tokio::test]
async fn callback_requires_the_browser_binding_cookie() {
    // Deliberately no Google mocks at all: if any of these reached the token
    // endpoint the assertion below would not be enough - a 404 from wiremock
    // would also render a page. `received_requests` is empty or this is wrong.
    let server = MockServer::start().await;
    let app = app_with_oauth(&server).await;

    // A second flow, so we have a real cookie that belongs to a *different*
    // pending state.
    let other = start_flow(&app).await;

    for cookie in [
        None,
        Some(String::new()),
        Some(format!("{BINDING_COOKIE}=")),
        Some(format!("{BINDING_COOKIE}=deadbeef")),
        Some("some_other_cookie=value".to_owned()),
        // The real cookie - but from the other flow.
        Some(other.cookie.clone()),
    ] {
        let started = start_flow(&app).await;
        let headers: Vec<(&str, &[u8])> = cookie
            .as_ref()
            .map(|value| vec![(header::COOKIE.as_str(), value.as_bytes())])
            .unwrap_or_default();
        let response = get_with(
            &app,
            &format!(
                "/v1/auth/gmail/callback?state={}&code=4/0Aauth-code",
                started.state
            ),
            &headers,
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "cookie {cookie:?} must not complete a consent"
        );
        assert!(body_text(response).await.contains("expired"));
    }

    let accounts: i64 = sqlx::query_scalar("select count(*) from accounts")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(accounts, 0, "nothing may be bound without the cookie");
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "the code must not even be exchanged"
    );
}

/// EDGE (unicode / duplicates in the Cookie header).
#[tokio::test]
async fn the_binding_cookie_survives_a_messy_cookie_jar() {
    let server = MockServer::start().await;
    google_token(&server).mount(&server).await;
    google_profile("jatinsethi98@gmail.com")
        .mount(&server)
        .await;
    let app = app_with_oauth(&server).await;

    // Bytes that are not header text: `to_str` fails, the header is ignored,
    // and the request is refused rather than panicking.
    let started = start_flow(&app).await;
    let response = get_with(
        &app,
        &format!(
            "/v1/auth/gmail/callback?state={}&code=4/0Aauth-code",
            started.state
        ),
        &[(header::COOKIE.as_str(), &[b'a', b'=', 0xff])],
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // The real cookie buried among decoys, in a second Cookie header, with a
    // same-named impostor first.
    let started = start_flow(&app).await;
    let decoy = format!("theme=dark; {BINDING_COOKIE}=deadbeef");
    let real = format!("session=abc; {} ; other=1", started.cookie);
    let response = get_with(
        &app,
        &format!(
            "/v1/auth/gmail/callback?state={}&code=4/0Aauth-code",
            started.state
        ),
        &[
            (header::COOKIE.as_str(), decoy.as_bytes()),
            (header::COOKIE.as_str(), real.as_bytes()),
        ],
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a browser may send several Cookie headers and several same-named cookies"
    );
}

/// EDGE (two `start` calls interleaved): each flow keeps its own `state` and its
/// own cookie, and the pair cannot be crossed.
#[tokio::test]
async fn two_interleaved_starts_do_not_cross_wires() {
    let server = MockServer::start().await;
    google_token(&server).mount(&server).await;
    google_profile("jatinsethi98@gmail.com")
        .mount(&server)
        .await;
    let app = app_with_oauth(&server).await;

    let first = start_flow(&app).await;
    let second = start_flow(&app).await;
    assert_ne!(first.state, second.state);
    assert_ne!(first.cookie, second.cookie);
    assert_eq!(app.state.gmail.pending.len(), 2);

    // Crossed: the second flow's cookie against the first flow's state.
    let crossed = get_with(
        &app,
        &format!(
            "/v1/auth/gmail/callback?state={}&code=4/0Aauth-code",
            first.state
        ),
        &[(header::COOKIE.as_str(), second.cookie.as_bytes())],
    )
    .await;
    assert_eq!(crossed.status(), StatusCode::BAD_REQUEST);

    // The second flow, matched properly, still completes.
    assert_eq!(complete(&app, &second).await.status(), StatusCode::OK);
}

/// Criterion K2a at the route: a `state` older than ten minutes is refused, and
/// the pending entry does not survive the attempt.
#[tokio::test]
async fn an_expired_pending_state_is_refused() {
    let server = MockServer::start().await;
    let app = app_with_oauth(&server).await;

    let Some(long_ago) = Instant::now().checked_sub(Duration::from_secs(601)) else {
        println!("skipped: the monotonic clock has not been running for ten minutes");
        return;
    };
    app.state.gmail.pending.remember_expiring_at(
        "stale-state".to_owned(),
        Pending {
            verifier: "verifier".to_owned(),
            binding: "cookie-value".to_owned(),
            device_id: None,
        },
        long_ago,
    );

    let response = get_with(
        &app,
        "/v1/auth/gmail/callback?state=stale-state&code=4/0Aauth-code",
        &[(
            header::COOKIE.as_str(),
            format!("{BINDING_COOKIE}=cookie-value").as_bytes(),
        )],
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(body_text(response).await.contains("expired"));
}

/// EDGE (a callback racing a device revoke): revoking a stolen token has to
/// close a consent that is already half way through Google.
#[tokio::test]
async fn a_callback_after_the_device_is_revoked_is_refused() {
    // No Google mocks: reaching the token endpoint at all would be the bug.
    let server = MockServer::start().await;
    let app = app_with_oauth(&server).await;

    let started = start_flow(&app).await;
    sqlx::query("update devices set revoked_at = now()")
        .execute(&app.db.pool)
        .await
        .unwrap();

    let response = complete(&app, &started).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let accounts: i64 = sqlx::query_scalar("select count(*) from accounts")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(accounts, 0, "a revoked device binds nothing");
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "and the authorization code is never spent"
    );
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

    let started = start_flow(&app).await;
    let response = complete(&app, &started).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(body_text(response).await.contains("already connected"));

    let emails: Vec<String> = sqlx::query_scalar("select email from accounts")
        .fetch_all(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(emails, vec!["jatinsethi98@gmail.com"]);
}

/// F2 at the route, deterministically: the loser of the **in-transaction** race
/// gets the same 409 page, not a 500 and not a second account.
///
/// The window the defect lived in is between `existing_account_email` (which
/// sees nothing) and `save_consent` (which writes). The test opens that window
/// by hand: it takes [`ACCOUNT_SINGLETON_LOCK`] itself, lets the callback sail
/// past the cheap pre-check and block inside `save_consent`, then binds a
/// *different* mailbox and commits. When the callback finally gets the lock the
/// world has changed underneath it - which is exactly the race, made repeatable.
///
/// If the advisory lock were ever removed, the callback would not block at all,
/// `wait_for` would time out, and this test would fail loudly.
#[tokio::test]
async fn a_callback_that_loses_the_bind_race_renders_the_conflict_page() {
    let server = MockServer::start().await;
    google_token(&server).mount(&server).await;
    google_profile("impostor@gmail.com").mount(&server).await;
    let app = app_with_oauth(&server).await;

    let started = start_flow(&app).await;

    // Hold the singleton lock, so any `save_consent` stops dead.
    let mut holder = app.db.pool.begin().await.unwrap();
    sqlx::query("select pg_advisory_xact_lock($1)")
        .bind(ACCOUNT_SINGLETON_LOCK)
        .execute(&mut *holder)
        .await
        .unwrap();

    let router = app.router.clone();
    let uri = format!(
        "/v1/auth/gmail/callback?state={}&code=4/0Aauth-code",
        started.state
    );
    let cookie = started.cookie.clone();
    let callback = tokio::spawn(async move {
        router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    });

    // It has cleared the pre-check and is now waiting on the lock. Scoped to
    // this test's own database, because the whole suite shares one cluster.
    let pool = app.db.pool.clone();
    crate::test_support::wait_for(Duration::from_secs(20), || {
        let pool = pool.clone();
        async move {
            let waiting: i64 = sqlx::query_scalar(
                "select count(*) from pg_locks \
                  where locktype = 'advisory' and not granted \
                    and database = (select oid from pg_database \
                                     where datname = current_database())",
            )
            .fetch_one(&pool)
            .await
            .unwrap_or(0);
            waiting > 0
        }
    })
    .await;

    // Somebody else binds the mailbox first, and commits.
    sqlx::query("insert into accounts (email) values ('jatinsethi98@gmail.com')")
        .execute(&mut *holder)
        .await
        .unwrap();
    holder.commit().await.unwrap();

    let response = callback.await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "the loser gets the 409 page, not a 500 and not a second mailbox"
    );
    assert!(body_text(response).await.contains("already connected"));

    let emails: Vec<String> = sqlx::query_scalar("select email from accounts")
        .fetch_all(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(emails, vec!["jatinsethi98@gmail.com"]);
    let tokens: i64 = sqlx::query_scalar("select count(*) from gmail_tokens")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(tokens, 0, "the losing transaction rolled back whole");
}

/// A server without `secrets/web_client.json` must still boot and say so - and
/// it must say so only to somebody who could have started a link.
#[tokio::test]
async fn an_unconfigured_server_explains_itself() {
    let app = test_app(Env::Prod).await;
    assert!(app.state.gmail.oauth.is_none());

    // The capability gate comes first, so an unauthorised caller cannot even
    // fingerprint the server's configuration.
    assert_eq!(
        authed_get(&app, "/v1/auth/gmail/start", None)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );

    let cap = capability(&app).await;
    let response = authed_get(&app, &format!("/v1/auth/gmail/start?cap={cap}"), None).await;
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

    let started = start_flow(&app).await;
    let response = get_with(
        &app,
        &format!("/v1/auth/gmail/callback?state={}&code=stale", started.state),
        &[(header::COOKIE.as_str(), started.cookie.as_bytes())],
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

// ------------------------------------------------------------- URL shapes --

#[test]
fn the_start_url_and_cookie_are_derived_from_the_registered_redirect() {
    // The normal case: the URL Google is configured with.
    assert_eq!(
        start_url("http://localhost:8080/v1/auth/gmail/callback", "cafe"),
        "http://localhost:8080/v1/auth/gmail/start?cap=cafe"
    );
    assert_eq!(
        cookie_path("http://localhost:8080/v1/auth/gmail/callback"),
        "/v1/auth/gmail"
    );

    // Behind a path-rewriting proxy.
    assert_eq!(
        start_url(
            "https://nade.example.com/api/v1/auth/gmail/callback",
            "beef"
        ),
        "https://nade.example.com/api/v1/auth/gmail/start?cap=beef"
    );
    assert_eq!(
        cookie_path("https://nade.example.com/api/v1/auth/gmail/callback"),
        "/api/v1/auth/gmail"
    );

    // A hand-set redirect that does not end in `/callback`: fall back to this
    // server's own route on the same origin rather than inventing a path.
    assert_eq!(
        start_url("https://nade.example.com/cb", "f00d"),
        "https://nade.example.com/v1/auth/gmail/start?cap=f00d"
    );
    // EDGE (empty input): nothing here may panic.
    assert_eq!(cookie_path(""), "/");
    assert_eq!(cookie_path("https://example.com"), "/");
    assert_eq!(cookie_path("https://example.com/callback"), "/");
    assert!(!start_url("", "x").is_empty());
}

#[test]
fn the_cookie_is_secure_only_where_secure_works() {
    let plain = binding_cookie("v", "http://localhost:8080/v1/auth/gmail/callback");
    assert!(
        !plain.contains("Secure"),
        "a Secure cookie is dropped over http, which would break dev: {plain}"
    );

    let tls = binding_cookie("v", "https://nade.example.com/v1/auth/gmail/callback");
    assert!(tls.contains("; Secure"), "{tls}");
    assert!(tls.contains("HttpOnly"), "{tls}");
    assert!(tls.contains("SameSite=Lax"), "{tls}");
}
