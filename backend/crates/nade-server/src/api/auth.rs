//! Device pairing and the bearer guard on `/v1/*`.

pub mod pairing;

use std::{
    collections::VecDeque,
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::{
    extract::{FromRequestParts, Request, State},
    http::{header, request::Parts, HeaderMap},
    middleware::Next,
    response::Response,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiJson, ApiResult},
    state::AppState,
};
use pairing::Consumed;

/// Long enough for "Jatin's iPhone 17 Pro 📱", short enough to be a name.
const MAX_DEVICE_NAME_CHARS: usize = 128;

/// The synthetic device behind the `NADE_TOKEN` dev shortcut. A real paired
/// device always has a random uuid, so this can never collide with one.
pub const DEV_DEVICE_ID: Uuid = Uuid::nil();

// ------------------------------------------------------------- principals --

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Device {
    pub id: Uuid,
    pub account_id: Option<Uuid>,
    pub device_name: String,
    pub bearer_hash: String,
}

impl Device {
    fn dev() -> Self {
        Self {
            id: DEV_DEVICE_ID,
            account_id: None,
            device_name: "dev-token".to_owned(),
            bearer_hash: String::new(),
        }
    }

    #[must_use]
    pub fn is_dev_token(&self) -> bool {
        self.id == DEV_DEVICE_ID
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Account {
    pub id: Uuid,
    pub email: String,
    pub status: String,
}

/// Who is making this request. Put in the request extensions by
/// [`require_bearer`]; P2+ handlers pull it out with the extractor below.
///
/// `account` is `None` until Gmail OAuth runs (P2) - a device pairs first and
/// gets a mailbox later.
#[derive(Debug, Clone)]
pub struct Auth {
    pub device: Device,
    pub account: Option<Account>,
}

impl<S> FromRequestParts<S> for Auth
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Self>()
            .cloned()
            .ok_or_else(ApiError::unauthorized)
    }
}

// ------------------------------------------------------------ rate limiter --

/// A fixed-count sliding window over the whole process.
///
/// EDGE (clock skew): built on `Instant`, which is monotonic, so moving the
/// wall clock cannot widen or reset the window.
#[derive(Debug)]
pub struct RateLimiter {
    max: u32,
    window: Duration,
    hits: Mutex<VecDeque<Instant>>,
}

impl RateLimiter {
    #[must_use]
    pub fn new(max: u32, window: Duration) -> Self {
        Self {
            max,
            window,
            hits: Mutex::new(VecDeque::new()),
        }
    }

    /// Record an attempt. `false` means the caller is over the limit.
    pub fn allow(&self) -> bool {
        let now = Instant::now();
        let mut hits = self
            .hits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while hits
            .front()
            .is_some_and(|t| now.duration_since(*t) >= self.window)
        {
            hits.pop_front();
        }
        if hits.len() as u32 >= self.max {
            return false;
        }
        hits.push_back(now);
        true
    }
}

// -------------------------------------------------------------- POST /pair --

#[derive(Debug, Deserialize)]
pub struct PairRequest {
    pub code: String,
    pub device_name: String,
}

/// `docs/contract/pair.json`.
#[derive(Debug, Serialize)]
pub struct PairResponse {
    pub token: String,
}

/// `POST /v1/auth/pair` - trade the console's 6-digit code for a bearer token.
///
/// # Errors
/// `429` when pairing is being hammered, `400` for an unusable `device_name`,
/// `401` for any bad, expired, or already-spent code.
pub async fn pair(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<PairRequest>,
) -> ApiResult<Json<PairResponse>> {
    // First, before any comparison: a 6-digit secret is only safe if guesses
    // are expensive. 10 attempts/minute needs ~50 days to sweep the space, and
    // the code lives for 10 minutes.
    if !state.pair_limiter.allow() {
        tracing::warn!("pairing rate limit tripped");
        return Err(ApiError::rate_limited());
    }

    let device_name = request.device_name.trim();
    // EDGE (empty input).
    if device_name.is_empty() {
        return Err(ApiError::bad_request("device_name must not be empty."));
    }
    // EDGE (unicode): count characters, not bytes, so a name of emoji is not
    // penalised for their width.
    if device_name.chars().count() > MAX_DEVICE_NAME_CHARS {
        return Err(ApiError::bad_request(format!(
            "device_name must be at most {MAX_DEVICE_NAME_CHARS} characters."
        )));
    }
    // EDGE (unicode): PostgreSQL `text` cannot hold a NUL byte; reject it as a
    // bad request instead of letting the driver raise a 500.
    if device_name.contains('\0') {
        return Err(ApiError::bad_request(
            "device_name must not contain a NUL byte.",
        ));
    }

    if state.pairing.consume(request.code.trim()).await != Consumed::Accepted {
        return Err(ApiError::unauthorized());
    }

    let token =
        pairing::random_token().map_err(|e| ApiError::internal("minting a device token", &e))?;
    let bearer_hash = pairing::token_hash(&token);

    // One transaction: a device row exists if and only if its audit row does.
    let mut tx = state.pool.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "insert into devices (bearer_hash, device_name, last_seen_at) \
         values ($1, $2, now()) returning id",
    )
    .bind(&bearer_hash)
    .bind(device_name)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("insert into audit_log (actor, action, subject) values ($1, 'device_paired', $2)")
        .bind(format!("device:{id}"))
        .bind(json!({ "device_id": id, "device_name": device_name }))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    tracing::info!(device_id = %id, device_name, "device paired");
    Ok(Json(PairResponse { token }))
}

// ------------------------------------------------------------- middleware --

/// Bearer guard for every `/v1` route except `healthz` and `auth/pair`.
///
/// On success the [`Auth`] goes into the request extensions; on failure the
/// caller gets the standard 401 envelope and never learns why.
///
/// # Errors
/// `401` when the header is missing, malformed, unknown, or revoked.
pub async fn require_bearer(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let presented = bearer(request.headers())
        .ok_or_else(ApiError::unauthorized)?
        .to_owned();
    let auth = authenticate(&state, &presented).await?;
    request.extensions_mut().insert(auth);
    Ok(next.run(request).await)
}

/// Pull `Authorization: Bearer <token>` apart.
///
/// EDGE (unicode): a header with non-ASCII bytes fails `to_str` and is simply
/// unauthorized - never a panic and never a 500.
/// EDGE (empty input): `Bearer` with nothing after it is unauthorized too.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, value) = raw.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

async fn authenticate(state: &AppState, presented: &str) -> Result<Auth, ApiError> {
    // The dev shortcut. `usable_dev_token()` is the single place that consults
    // NADE_ENV, and it hands back `None` unless the env is exactly `dev`, so
    // there is no reachable path to this branch in production.
    if let Some(expected) = state.config.usable_dev_token() {
        if bool::from(expected.as_bytes().ct_eq(presented.as_bytes())) {
            return Ok(Auth {
                device: Device::dev(),
                account: singleton_account(state).await?,
            });
        }
    }

    let hash = pairing::token_hash(presented);
    let device: Device = sqlx::query_as(
        "update devices set last_seen_at = now() \
          where bearer_hash = $1 and revoked_at is null \
         returning id, account_id, device_name, bearer_hash",
    )
    .bind(&hash)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(ApiError::unauthorized)?;

    // The index lookup already matched the full hash; comparing again in
    // constant time is what stops a future refactor (a prefix search, a LIKE)
    // from quietly becoming a timing oracle.
    if !bool::from(device.bearer_hash.as_bytes().ct_eq(hash.as_bytes())) {
        return Err(ApiError::unauthorized());
    }

    let account = match device.account_id {
        Some(id) => {
            sqlx::query_as("select id, email, status from accounts where id = $1")
                .bind(id)
                .fetch_optional(&state.pool)
                .await?
        }
        None => None,
    };

    Ok(Auth { device, account })
}

/// v1 is single-account (PLAN.md §Design parity map), so the dev token adopts
/// whichever account exists. At P1 there is none yet.
///
/// `order by created_at, id` for the same reason as [`AppState::account`]:
/// `created_at` alone is not a total order, and "the account" must be the same
/// row every time it is asked for.
async fn singleton_account(state: &AppState) -> Result<Option<Account>, ApiError> {
    Ok(
        sqlx::query_as("select id, email, status from accounts order by created_at, id limit 1")
            .fetch_optional(&state.pool)
            .await?,
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::http::StatusCode;

    use super::*;
    use crate::{
        config::Env,
        test_support::{fixture, get, post_json, response_json, test_app, TestApp},
    };

    async fn pair_with(app: &TestApp, code: &str, name: &str) -> Response {
        post_json(
            app,
            "/v1/auth/pair",
            &json!({ "code": code, "device_name": name }),
        )
        .await
    }

    /// Criterion F2.
    #[tokio::test]
    async fn pair_returns_a_contract_shaped_token() {
        let app = test_app(Env::Prod).await;
        let code = app.state.pairing.mint().await.unwrap();

        let response = pair_with(&app, &code.code, "jatin-iphone").await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;

        // The fixture's token is an example value - a per-device secret can
        // never be byte-matched - so we assert its key set and its form.
        let expected = fixture("pair.json");
        assert_eq!(
            body.as_object().unwrap().keys().collect::<Vec<_>>(),
            expected.as_object().unwrap().keys().collect::<Vec<_>>()
        );
        let token = body["token"].as_str().unwrap();
        let example = expected["token"].as_str().unwrap();
        assert!(token.starts_with("nade_"), "{token}");
        assert!(example.starts_with("nade_"));
        assert!(
            token
                .strip_prefix("nade_")
                .unwrap()
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "{token}"
        );
    }

    /// Criterion F3 + F13.
    #[tokio::test]
    async fn only_the_token_hash_is_persisted() {
        let app = test_app(Env::Prod).await;
        let code = app.state.pairing.mint().await.unwrap();
        let body = response_json(pair_with(&app, &code.code, "jatin-iphone").await).await;
        let token = body["token"].as_str().unwrap().to_owned();

        let (hash, name): (String, String) =
            sqlx::query_as("select bearer_hash, device_name from devices")
                .fetch_one(&app.db.pool)
                .await
                .unwrap();
        assert_eq!(hash, pairing::token_hash(&token));
        assert_eq!(name, "jatin-iphone");

        // The plaintext must appear nowhere in the row.
        let dump: String =
            sqlx::query_scalar("select coalesce(string_agg(devices::text, ' '), '') from devices")
                .fetch_one(&app.db.pool)
                .await
                .unwrap();
        assert!(!dump.contains(&token), "the plaintext token was stored");
    }

    #[tokio::test]
    async fn pairing_writes_an_audit_row() {
        let app = test_app(Env::Prod).await;
        let code = app.state.pairing.mint().await.unwrap();
        pair_with(&app, &code.code, "jatin-iphone").await;

        let (actor, action): (String, String) =
            sqlx::query_as("select actor, action from audit_log")
                .fetch_one(&app.db.pool)
                .await
                .unwrap();
        assert!(actor.starts_with("device:"), "{actor}");
        assert_eq!(action, "device_paired");
    }

    /// Criterion F4 - replay. PLAN.md P1 acceptance: "second use 401".
    #[tokio::test]
    async fn a_consumed_code_is_rejected() {
        let app = test_app(Env::Prod).await;
        let code = app.state.pairing.mint().await.unwrap();

        assert_eq!(
            pair_with(&app, &code.code, "first").await.status(),
            StatusCode::OK
        );
        let second = pair_with(&app, &code.code, "second").await;
        assert_eq!(second.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response_json(second).await,
            fixture("error_unauthorized.json")
        );

        let devices: i64 = sqlx::query_scalar("select count(*) from devices")
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
        assert_eq!(devices, 1);
    }

    /// Criterion F5.
    #[tokio::test]
    async fn a_wrong_code_is_rejected() {
        let app = test_app(Env::Prod).await;
        let code = app.state.pairing.mint().await.unwrap();
        let wrong = format!("{:06}", (code.code.parse::<u32>().unwrap() + 1) % 1_000_000);

        let response = pair_with(&app, &wrong, "impostor").await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response_json(response).await,
            fixture("error_unauthorized.json")
        );
        // The real code survives a wrong guess.
        assert_eq!(
            pair_with(&app, &code.code, "jatin-iphone").await.status(),
            StatusCode::OK
        );
    }

    /// Criterion F6.
    #[tokio::test]
    async fn an_expired_code_is_rejected() {
        let mut app = test_app(Env::Prod).await;
        app.set_pairing_ttl(Duration::from_millis(1));
        let code = app.state.pairing.mint().await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;

        assert_eq!(
            pair_with(&app, &code.code, "late").await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    /// Criterion F7.
    #[tokio::test]
    async fn pairing_attempts_are_rate_limited() {
        let mut app = test_app(Env::Prod).await;
        app.set_pair_rate_limit(3, Duration::from_secs(60));
        app.state.pairing.mint().await.unwrap();

        for attempt in 1..=3 {
            assert_eq!(
                pair_with(&app, "000000", "guesser").await.status(),
                StatusCode::UNAUTHORIZED,
                "attempt {attempt}"
            );
        }
        let blocked = pair_with(&app, "000000", "guesser").await;
        assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = response_json(blocked).await;
        assert_eq!(body["error"]["code"], "rate_limited");
    }

    #[test]
    fn rate_limiter_lets_the_window_slide() {
        let limiter = RateLimiter::new(2, Duration::from_millis(80));
        assert!(limiter.allow());
        assert!(limiter.allow());
        assert!(!limiter.allow());
        std::thread::sleep(Duration::from_millis(120));
        assert!(limiter.allow(), "the window should have slid");
    }

    /// Edge case 1 + 2.
    #[tokio::test]
    async fn empty_or_oversized_device_name_is_rejected() {
        let app = test_app(Env::Prod).await;
        let code = app.state.pairing.mint().await.unwrap();

        for name in ["", "   ", "\t\n"] {
            let response = pair_with(&app, &code.code, name).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{name:?}");
            assert_eq!(
                response_json(response).await["error"]["code"],
                "bad_request"
            );
        }

        let long = "x".repeat(MAX_DEVICE_NAME_CHARS + 1);
        assert_eq!(
            pair_with(&app, &code.code, &long).await.status(),
            StatusCode::BAD_REQUEST
        );

        // A NUL byte would be a driver-level 500 if we let it through.
        assert_eq!(
            pair_with(&app, &code.code, "bad\0name").await.status(),
            StatusCode::BAD_REQUEST
        );

        // None of that spent the code.
        assert_eq!(
            pair_with(&app, &code.code, "jatin-iphone").await.status(),
            StatusCode::OK
        );
    }

    /// Edge case 2 (unicode).
    #[tokio::test]
    async fn unicode_device_names_round_trip() {
        let app = test_app(Env::Prod).await;
        let code = app.state.pairing.mint().await.unwrap();
        let name = "ジャティンの iPhone 📱 — Café";

        assert_eq!(
            pair_with(&app, &code.code, name).await.status(),
            StatusCode::OK
        );
        let stored: String = sqlx::query_scalar("select device_name from devices")
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
        assert_eq!(stored, name);
        // 128 characters is a character limit, not a byte limit.
        assert!(name.len() > name.chars().count());
    }

    /// Criterion G5.
    #[tokio::test]
    async fn malformed_json_is_a_bad_request_envelope() {
        let app = test_app(Env::Prod).await;
        for (body, content_type) in [
            ("{not json", "application/json"),
            ("", "application/json"),
            ("{\"code\":\"123456\"}", "application/json"), // device_name missing
            ("{}", "text/plain"),
        ] {
            let response = app.raw_post("/v1/auth/pair", body, content_type).await;
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "{body:?} / {content_type}"
            );
            assert_eq!(
                response_json(response).await["error"]["code"],
                "bad_request"
            );
        }
    }

    /// Criterion F10.
    #[tokio::test]
    async fn dev_token_is_accepted_in_dev() {
        let mut app = test_app(Env::Dev).await;
        app.set_dev_token(Some("dev-secret".into()));

        let response = get(&app, "/v1/not-a-real-route", Some("dev-secret")).await;
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "the dev token should have got past the guard"
        );
    }

    /// Criterion F11 - the one that must never regress.
    #[tokio::test]
    async fn dev_token_is_impossible_outside_dev() {
        let mut app = test_app(Env::Prod).await;
        app.set_dev_token(Some("dev-secret".into()));

        let response = get(&app, "/v1/not-a-real-route", Some("dev-secret")).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response_json(response).await,
            fixture("error_unauthorized.json")
        );
    }

    /// Criterion F12.
    #[tokio::test]
    async fn a_revoked_device_is_rejected() {
        let app = test_app(Env::Prod).await;
        let code = app.state.pairing.mint().await.unwrap();
        let body = response_json(pair_with(&app, &code.code, "jatin-iphone").await).await;
        let token = body["token"].as_str().unwrap().to_owned();

        assert_eq!(
            get(&app, "/v1/not-a-real-route", Some(&token))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );

        sqlx::query("update devices set revoked_at = now()")
            .execute(&app.db.pool)
            .await
            .unwrap();

        assert_eq!(
            get(&app, "/v1/not-a-real-route", Some(&token))
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }

    /// Edge case 1 + 2 on the Authorization header itself.
    #[tokio::test]
    async fn missing_or_empty_authorization_is_unauthorized() {
        let app = test_app(Env::Prod).await;
        for header in [
            None,
            Some(""),
            Some("Bearer"),
            Some("Bearer   "),
            Some("Basic abc"),
        ] {
            let response = get(&app, "/v1/not-a-real-route", header).await;
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{header:?} must not authenticate"
            );
        }
    }

    #[tokio::test]
    async fn non_ascii_authorization_header_is_unauthorized() {
        let app = test_app(Env::Prod).await;
        // Bytes that are not valid header text: `to_str` fails and we reject.
        let response = app
            .raw_get(
                "/v1/not-a-real-route",
                Some(&[b'B', b'e', b'a', b'r', b'e', b'r', b' ', 0xff]),
            )
            .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn an_unknown_token_is_unauthorized() {
        let app = test_app(Env::Prod).await;
        let response = get(
            &app,
            "/v1/not-a-real-route",
            Some(&format!("nade_{}", "0".repeat(64))),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
