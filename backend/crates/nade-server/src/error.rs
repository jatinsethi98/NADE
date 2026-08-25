//! The one and only error shape this API emits.
//!
//! `{"error":{"code":"…","message":"…"}}` - PLAN.md §Canonical API Contract,
//! byte-checked against `docs/contract/error_unauthorized.json`.

use axum::{
    extract::{rejection::JsonRejection, FromRequest, Request},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{de::DeserializeOwned, Serialize};

pub type ApiResult<T> = std::result::Result<T, ApiError>;

/// Machine-readable error codes. P1 needs the first four; `rate_limited` is
/// the pairing brute-force guard (backend/DECISIONS.md D5). P5 added the four
/// the approval loop answers with — `conflict`, `token_consumed`, `gone` and
/// `approval_expired`. `forbidden` is the only code in `API.md` §0 with no
/// variant here, and deliberately: v1 is single-account and nothing serves a
/// 403, so a variant would exist to make a coverage test pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    BadRequest,
    Unauthorized,
    NotFound,
    /// The attachment proxy's 25 MB cap.
    PayloadTooLarge,
    RateLimited,
    /// Gmail credentials are dead; the user must re-consent (`API.md` §0).
    NeedsReauth,
    /// Gmail failed after retries. Never a 500: the fault is upstream, and the
    /// client's correct response (retry later) is different.
    UpstreamUnavailable,
    /// State moved on under the caller (`API.md` §0). P5's approve and skip
    /// raise it when a card's run is no longer parked on the step the card
    /// names.
    Conflict,
    /// This approval was already recorded. **Clients treat it as success** —
    /// it means an earlier attempt already won.
    TokenConsumed,
    /// The resource existed and is gone. P5 raises it for a card whose agent
    /// was deleted while the card was still live.
    Gone,
    /// This approval passed its seven-day deadline.
    ApprovalExpired,
    Internal,
}

impl ErrorCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BadRequest => "bad_request",
            Self::Unauthorized => "unauthorized",
            Self::NotFound => "not_found",
            Self::PayloadTooLarge => "payload_too_large",
            Self::RateLimited => "rate_limited",
            Self::NeedsReauth => "needs_reauth",
            Self::UpstreamUnavailable => "upstream_unavailable",
            Self::Conflict => "conflict",
            Self::TokenConsumed => "token_consumed",
            Self::Gone => "gone",
            Self::ApprovalExpired => "approval_expired",
            Self::Internal => "internal",
        }
    }

    #[must_use]
    pub const fn status(self) -> StatusCode {
        match self {
            Self::BadRequest => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::NeedsReauth => StatusCode::CONFLICT,
            Self::UpstreamUnavailable => StatusCode::BAD_GATEWAY,
            // `conflict` and `token_consumed` share a status and differ in
            // code, which is the whole point: one means "reload", the other
            // means "you already won".
            Self::Conflict | Self::TokenConsumed => StatusCode::CONFLICT,
            Self::Gone | Self::ApprovalExpired => StatusCode::GONE,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// The wording the contract fixtures use.
    #[must_use]
    pub const fn default_message(self) -> &'static str {
        match self {
            Self::BadRequest => "The request could not be understood.",
            // Verbatim from docs/contract/error_unauthorized.json.
            Self::Unauthorized => "Bearer token missing, unknown, or revoked.",
            Self::NotFound => "That does not exist.",
            // Verbatim from docs/contract/error_payload_too_large.json.
            Self::PayloadTooLarge => "That is too big to send. The limit is 25 MB.",
            Self::RateLimited => "Too many attempts. Wait a minute and try again.",
            // Verbatim from docs/contract/error_needs_reauth.json.
            Self::NeedsReauth => "Gmail needs to be reconnected. Sign in again in Settings.",
            // Verbatim from docs/contract/error_upstream_unavailable.json.
            Self::UpstreamUnavailable => {
                "Gmail didn't respond. Nothing was lost — try again in a moment."
            }
            // The four below are verbatim from their contract fixtures, and
            // `every_served_code_matches_its_contract_fixture` compares the
            // message as well as the code (D22).
            Self::Conflict => "Something changed while you were working. Reload and try again.",
            Self::TokenConsumed => "This approval was already recorded.",
            Self::Gone => "That is no longer available.",
            Self::ApprovalExpired => {
                "This approval expired after 7 days. Run the agent again to get a fresh one."
            }
            Self::Internal => "Something went wrong on the server.",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    /// Seconds until the caller may try again.
    ///
    /// `API.md` §0 makes the header part of `rate_limited`'s definition — "Too
    /// many attempts; `Retry-After` header set" — and the iOS client already
    /// reads it (`APIClient.swift` exposes `APIFailure.retryAfter`). Emitting
    /// the status without the header tells the app to wait with no idea how
    /// long, which for a daily budget is about 24 hours out.
    pub retry_after_secs: Option<u64>,
}

/// What a `429` says to wait when the caller names no figure of its own.
///
/// A minute: long enough that a client honouring it stops hammering, short
/// enough that a transient limit is not turned into an outage.
pub const DEFAULT_RETRY_AFTER_SECS: u64 = 60;

impl ApiError {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retry_after_secs: None,
        }
    }

    /// Attach `Retry-After`, in seconds.
    #[must_use]
    pub fn retry_after(mut self, secs: u64) -> Self {
        self.retry_after_secs = Some(secs);
        self
    }

    /// The code's canonical message - what the fixtures show.
    #[must_use]
    pub fn of(code: ErrorCode) -> Self {
        Self::new(code, code.default_message())
    }

    #[must_use]
    pub fn unauthorized() -> Self {
        Self::of(ErrorCode::Unauthorized)
    }

    #[must_use]
    pub fn not_found() -> Self {
        Self::of(ErrorCode::NotFound)
    }

    /// A 429 with no figure of its own. `IntoResponse` supplies
    /// [`DEFAULT_RETRY_AFTER_SECS`], so the header is never absent; a caller
    /// that knows better says so with [`Self::retry_after`].
    #[must_use]
    pub fn rate_limited() -> Self {
        Self::of(ErrorCode::RateLimited)
    }

    #[must_use]
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::BadRequest, message)
    }

    #[must_use]
    pub fn upstream_unavailable() -> Self {
        Self::of(ErrorCode::UpstreamUnavailable)
    }

    #[must_use]
    pub fn needs_reauth() -> Self {
        Self::of(ErrorCode::NeedsReauth)
    }

    #[must_use]
    pub fn payload_too_large() -> Self {
        Self::of(ErrorCode::PayloadTooLarge)
    }

    #[must_use]
    pub fn conflict() -> Self {
        Self::of(ErrorCode::Conflict)
    }

    #[must_use]
    pub fn token_consumed() -> Self {
        Self::of(ErrorCode::TokenConsumed)
    }

    #[must_use]
    pub fn gone() -> Self {
        Self::of(ErrorCode::Gone)
    }

    #[must_use]
    pub fn approval_expired() -> Self {
        Self::of(ErrorCode::ApprovalExpired)
    }

    /// Internal failures are logged in full and reported as a fixed string:
    /// the client learns nothing about our innards.
    #[must_use]
    pub fn internal(context: &str, detail: &dyn std::fmt::Display) -> Self {
        tracing::error!(error = %detail, "{context}");
        Self::of(ErrorCode::Internal)
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for ApiError {}

#[derive(Serialize)]
struct Envelope<'a> {
    error: EnvelopeBody<'a>,
}

#[derive(Serialize)]
struct EnvelopeBody<'a> {
    code: &'a str,
    message: &'a str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (
            self.code.status(),
            Json(Envelope {
                error: EnvelopeBody {
                    code: self.code.as_str(),
                    message: &self.message,
                },
            }),
        )
            .into_response();

        // `API.md` §0 writes the header into `rate_limited`'s own definition,
        // and the iOS client reads it. A 429 without it says "wait" and not
        // "wait this long", which for a daily budget is a day out.
        // The default is not cosmetic: `API.md` §0 defines `rate_limited` as
        // "Too many attempts; `Retry-After` header set", so a 429 emitted
        // without one does not match its own definition. Enforced here rather
        // than at each call site, which is how the pairing guard's 429 (D5) came
        // to ship bare while the agent budget's carried a figure.
        let retry_after = self
            .retry_after_secs
            .or((self.code == ErrorCode::RateLimited).then_some(DEFAULT_RETRY_AFTER_SECS));
        if let Some(secs) = retry_after {
            if let Ok(value) = axum::http::HeaderValue::from_str(&secs.to_string()) {
                response
                    .headers_mut()
                    .insert(axum::http::header::RETRY_AFTER, value);
            }
        }
        response
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        Self::internal("database call failed", &err)
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        Self::internal("request failed", &format!("{err:#}"))
    }
}

/// `axum::Json` whose rejections land in our envelope instead of axum's
/// plain-text default.
///
/// EDGE (empty input / unicode): an empty body, a truncated body, a wrong
/// content-type, or invalid UTF-8 all become one 400 `bad_request` envelope.
pub struct ApiJson<T>(pub T);

impl<T, S> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(ApiError::bad_request(match rejection {
                JsonRejection::MissingJsonContentType(_) => {
                    "Expected a `content-type: application/json` request.".to_owned()
                }
                other => other.body_text(),
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    /// `API.md` §0 defines `rate_limited` as "Too many attempts; `Retry-After`
    /// header set", so a 429 without one does not match its own definition.
    /// The pairing guard's 429 (D5) shipped bare while the agent budget's
    /// carried a figure; the default closes that by construction.
    #[test]
    fn every_rate_limited_response_carries_retry_after() {
        let bare = ApiError::rate_limited().into_response();
        assert_eq!(bare.status(), ErrorCode::RateLimited.status());
        assert_eq!(
            bare.headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some(DEFAULT_RETRY_AFTER_SECS.to_string().as_str()),
        );

        // A caller that knows better still wins.
        let explicit = ApiError::rate_limited().retry_after(3_600).into_response();
        assert_eq!(
            explicit
                .headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some("3600"),
        );

        // And nothing else grows the header.
        for error in [
            ApiError::bad_request("no"),
            ApiError::unauthorized(),
            ApiError::not_found(),
        ] {
            let response = error.into_response();
            assert!(
                response
                    .headers()
                    .get(axum::http::header::RETRY_AFTER)
                    .is_none(),
                "only a 429 carries Retry-After"
            );
        }
    }

    use crate::test_support::{fixture, response_json};

    /// Criterion P1 - every code we serve matches its `docs/contract/` fixture,
    /// message and all. The wording is the *user's* only explanation of what
    /// went wrong, so it is part of the contract rather than a detail.
    #[tokio::test]
    async fn every_served_code_matches_its_contract_fixture() {
        for code in [
            ErrorCode::BadRequest,
            ErrorCode::Unauthorized,
            ErrorCode::NotFound,
            ErrorCode::PayloadTooLarge,
            ErrorCode::RateLimited,
            ErrorCode::NeedsReauth,
            ErrorCode::UpstreamUnavailable,
            // P5's four. `error_forbidden.json` stays deferred: v1 is
            // single-account and nothing serves `403`.
            ErrorCode::Conflict,
            ErrorCode::TokenConsumed,
            ErrorCode::Gone,
            ErrorCode::ApprovalExpired,
            ErrorCode::Internal,
        ] {
            let name = format!("error_{}.json", code.as_str());
            let response = ApiError::of(code).into_response();
            assert_eq!(response.status().as_u16(), code.status().as_u16(), "{name}");
            assert_eq!(
                response_json(response).await,
                fixture(&name),
                "{name} and ErrorCode::{code:?} disagree; the fixture is the contract"
            );
        }
    }

    #[tokio::test]
    async fn unauthorized_matches_the_contract_fixture() {
        let response = ApiError::unauthorized().into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers().get(axum::http::header::CONTENT_TYPE),
            Some(&axum::http::HeaderValue::from_static("application/json"))
        );
        assert_eq!(
            response_json(response).await,
            fixture("error_unauthorized.json")
        );
    }

    #[test]
    fn status_codes_match_the_codes() {
        for (code, expected, wire) in [
            (ErrorCode::BadRequest, 400, "bad_request"),
            (ErrorCode::Unauthorized, 401, "unauthorized"),
            (ErrorCode::NotFound, 404, "not_found"),
            (ErrorCode::PayloadTooLarge, 413, "payload_too_large"),
            (ErrorCode::RateLimited, 429, "rate_limited"),
            (ErrorCode::NeedsReauth, 409, "needs_reauth"),
            (ErrorCode::UpstreamUnavailable, 502, "upstream_unavailable"),
            (ErrorCode::Conflict, 409, "conflict"),
            (ErrorCode::TokenConsumed, 409, "token_consumed"),
            (ErrorCode::Gone, 410, "gone"),
            (ErrorCode::ApprovalExpired, 410, "approval_expired"),
            (ErrorCode::Internal, 500, "internal"),
        ] {
            assert_eq!(code.status().as_u16(), expected, "{wire}");
            assert_eq!(code.as_str(), wire);
        }
    }

    #[tokio::test]
    async fn envelope_shape_is_exactly_code_and_message() {
        for code in [
            ErrorCode::BadRequest,
            ErrorCode::Unauthorized,
            ErrorCode::NotFound,
            ErrorCode::PayloadTooLarge,
            ErrorCode::RateLimited,
            ErrorCode::NeedsReauth,
            ErrorCode::UpstreamUnavailable,
            ErrorCode::Conflict,
            ErrorCode::TokenConsumed,
            ErrorCode::Gone,
            ErrorCode::ApprovalExpired,
            ErrorCode::Internal,
        ] {
            let body = response_json(ApiError::of(code).into_response()).await;
            let top = body.as_object().expect("object");
            assert_eq!(top.len(), 1, "top level must hold only `error`: {body}");
            let inner = top["error"].as_object().expect("error object");
            assert_eq!(inner.len(), 2, "error must hold only code+message: {body}");
            assert_eq!(inner["code"], code.as_str());
            assert_eq!(inner["message"], code.default_message());
        }
    }

    /// Internal errors must not echo the underlying failure to the caller.
    #[tokio::test]
    async fn internal_errors_do_not_leak_detail() {
        let err = ApiError::internal("test", &"connection string: postgres://u:p@h/db");
        let body = response_json(err.into_response()).await;
        let rendered = body.to_string();
        assert!(!rendered.contains("postgres://"), "{rendered}");
        assert_eq!(
            body["error"]["message"],
            ErrorCode::Internal.default_message()
        );
    }
}
