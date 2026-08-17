//! Gmail's error bodies, reproduced rather than approximated.
//!
//! A client's retry policy is written against these bodies: `429` and
//! `403 rateLimitExceeded` mean different things and want different backoff,
//! `403 userRateLimitExceeded` is a third, and `401 authError` means "refresh,
//! then retry once" while `invalid_grant` on the refresh means "stop and ask the
//! user". A simulator that returned a bare status code with an empty body would
//! let a client that reads none of this pass every test.

use serde_json::{json, Value};

/// One of Gmail's error conditions, with its exact wire shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// `400` — a malformed parameter. Carries the message Gmail puts in it.
    InvalidArgument(String),
    /// `401` — the access token is missing, wrong or expired.
    InvalidCredentials,
    /// `403` — the *project's* rate limit. Backoff helps; it is not per-user.
    RateLimitExceeded,
    /// `403` — the per-user rate limit. This is the one a 250-units/second
    /// mailbox actually hits.
    UserRateLimitExceeded,
    /// `403` — the OAuth grant does not cover this call.
    InsufficientPermission,
    /// `404` — no such message, thread, label, or a `startHistoryId` that has
    /// aged out of the history window.
    NotFound,
    /// `429` — the newer shape of "too many requests". Gmail uses both this and
    /// the 403s; a client must handle all of them.
    TooManyRequests,
    /// `500`.
    BackendError,
    /// `503`.
    ServiceUnavailable,
}

impl ApiError {
    /// The HTTP status code.
    #[must_use]
    pub fn status(&self) -> u16 {
        match self {
            Self::InvalidArgument(_) => 400,
            Self::InvalidCredentials => 401,
            Self::RateLimitExceeded
            | Self::UserRateLimitExceeded
            | Self::InsufficientPermission => 403,
            Self::NotFound => 404,
            Self::TooManyRequests => 429,
            Self::BackendError => 500,
            Self::ServiceUnavailable => 503,
        }
    }

    /// Google's `error.errors[].reason`, which is what a retry policy switches on.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Self::InvalidArgument(_) => "invalidArgument",
            Self::InvalidCredentials => "authError",
            Self::RateLimitExceeded | Self::TooManyRequests => "rateLimitExceeded",
            Self::UserRateLimitExceeded => "userRateLimitExceeded",
            Self::InsufficientPermission => "insufficientPermissions",
            Self::NotFound => "notFound",
            Self::BackendError | Self::ServiceUnavailable => "backendError",
        }
    }

    /// Google's canonical-code `error.status`.
    #[must_use]
    pub fn canonical_status(&self) -> &'static str {
        match self {
            Self::InvalidArgument(_) => "INVALID_ARGUMENT",
            Self::InvalidCredentials => "UNAUTHENTICATED",
            Self::RateLimitExceeded
            | Self::UserRateLimitExceeded
            | Self::InsufficientPermission => "PERMISSION_DENIED",
            Self::NotFound => "NOT_FOUND",
            Self::TooManyRequests => "RESOURCE_EXHAUSTED",
            Self::BackendError => "INTERNAL",
            Self::ServiceUnavailable => "UNAVAILABLE",
        }
    }

    fn domain(&self) -> &'static str {
        match self {
            Self::RateLimitExceeded | Self::UserRateLimitExceeded => "usageLimits",
            _ => "global",
        }
    }

    /// The top-level `error.message`.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::InvalidArgument(detail) => detail.clone(),
            Self::InvalidCredentials => "Request had invalid authentication credentials. Expected OAuth 2 access token, login cookie or other valid authentication credential. See https://developers.google.com/identity/sign-in/web/devconsole-project.".to_owned(),
            Self::RateLimitExceeded => "Rate Limit Exceeded".to_owned(),
            Self::UserRateLimitExceeded => "User-rate limit exceeded.  Retry after the time indicated in the Retry-After header.".to_owned(),
            Self::InsufficientPermission => "Request had insufficient authentication scopes.".to_owned(),
            Self::NotFound => "Requested entity was not found.".to_owned(),
            Self::TooManyRequests => "Resource has been exhausted (e.g. check quota).".to_owned(),
            Self::BackendError => "Backend Error".to_owned(),
            Self::ServiceUnavailable => "The service is currently unavailable.".to_owned(),
        }
    }

    /// The short message Google repeats inside `error.errors[]`, which is not
    /// always the same string as `error.message`.
    fn short_message(&self) -> String {
        match self {
            Self::InvalidCredentials => "Invalid Credentials".to_owned(),
            Self::UserRateLimitExceeded => "User Rate Limit Exceeded".to_owned(),
            Self::NotFound => "Not Found".to_owned(),
            Self::ServiceUnavailable => "Backend Error".to_owned(),
            other => other.message(),
        }
    }

    /// The full `{"error": …}` document.
    #[must_use]
    pub fn body(&self) -> Value {
        let mut detail = json!({
            "message": self.short_message(),
            "domain": self.domain(),
            "reason": self.reason(),
        });
        // Only the 401 carries a location, and a client that keys off it is
        // relying on something real.
        if matches!(self, Self::InvalidCredentials) {
            detail["location"] = json!("Authorization");
            detail["locationType"] = json!("header");
        }
        json!({
            "error": {
                "code": self.status(),
                "message": self.message(),
                "errors": [detail],
                "status": self.canonical_status(),
            }
        })
    }
}

/// Errors this crate raises at *its own* boundary — bad seeds, bad specs, a
/// scenario that names a message that does not exist. Never an API error: those
/// are [`ApiError`] and travel on the wire.
#[derive(Debug, thiserror::Error)]
pub enum SimError {
    /// A mutation named a message that is not in the mailbox.
    #[error("no such message: {0}")]
    NoSuchMessage(String),
    /// A mutation named a label that is not in the mailbox.
    #[error("no such label: {0}")]
    NoSuchLabel(String),
    /// A label name collides with an existing one, which Gmail rejects.
    #[error("a label named {0:?} already exists")]
    DuplicateLabel(String),
    /// A system label cannot be created, renamed or deleted.
    #[error("{0} is a system label and cannot be modified")]
    SystemLabelIsImmutable(String),
    /// Reading a seed corpus failed for a reason other than "it is not there".
    #[error("seeding from {path}: {source}")]
    Seed {
        /// The path that failed.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A message could not be built from a spec.
    #[error("invalid message spec: {0}")]
    InvalidSpec(String),
}

/// The crate's result alias.
pub type Result<T, E = SimError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_403s_are_distinguishable() {
        let project = ApiError::RateLimitExceeded.body();
        let user = ApiError::UserRateLimitExceeded.body();
        assert_eq!(project["error"]["code"], 403);
        assert_eq!(user["error"]["code"], 403);
        assert_eq!(project["error"]["errors"][0]["reason"], "rateLimitExceeded");
        assert_eq!(
            user["error"]["errors"][0]["reason"],
            "userRateLimitExceeded"
        );
        assert_ne!(project, user);
    }

    #[test]
    fn a_429_is_not_a_403() {
        let flood = ApiError::TooManyRequests;
        assert_eq!(flood.status(), 429);
        assert_eq!(flood.canonical_status(), "RESOURCE_EXHAUSTED");
        // Shares the reason with the 403, which is exactly why a client must
        // switch on the status *and* the reason.
        assert_eq!(flood.reason(), ApiError::RateLimitExceeded.reason());
    }

    #[test]
    fn the_401_names_the_authorization_header() {
        let body = ApiError::InvalidCredentials.body();
        assert_eq!(body["error"]["code"], 401);
        assert_eq!(body["error"]["status"], "UNAUTHENTICATED");
        assert_eq!(body["error"]["errors"][0]["reason"], "authError");
        assert_eq!(body["error"]["errors"][0]["location"], "Authorization");
        assert_eq!(body["error"]["errors"][0]["message"], "Invalid Credentials");
    }

    #[test]
    fn every_error_body_has_googles_four_top_level_fields() {
        for error in [
            ApiError::InvalidArgument("bad".into()),
            ApiError::InvalidCredentials,
            ApiError::RateLimitExceeded,
            ApiError::UserRateLimitExceeded,
            ApiError::InsufficientPermission,
            ApiError::NotFound,
            ApiError::TooManyRequests,
            ApiError::BackendError,
            ApiError::ServiceUnavailable,
        ] {
            let body = error.body();
            let object = body["error"].as_object().expect("error is an object");
            assert_eq!(object.len(), 4, "{error:?}");
            for key in ["code", "message", "errors", "status"] {
                assert!(object.contains_key(key), "{error:?} is missing {key}");
            }
            assert!(body["error"]["errors"][0]["reason"].is_string());
        }
    }
}
