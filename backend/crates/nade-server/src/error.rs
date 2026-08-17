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
/// the pairing brute-force guard (backend/DECISIONS.md D5). Later phases add
/// `token_consumed` and `approval_expired`, which already have fixtures.
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
            Self::Internal => "Something went wrong on the server.",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
}

impl ApiError {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
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
        (
            self.code.status(),
            Json(Envelope {
                error: EnvelopeBody {
                    code: self.code.as_str(),
                    message: &self.message,
                },
            }),
        )
            .into_response()
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
