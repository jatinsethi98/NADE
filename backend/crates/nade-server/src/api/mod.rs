//! The `/v1` router.
//!
//! Two public routes, and then *everything else* under `/v1` - including paths
//! that do not exist yet - goes through the bearer guard. That ordering is
//! deliberate: a route added in P2 is protected the moment it is written, and
//! forgetting to guard it is not a thing that can happen.

pub mod auth;
pub mod health;

use std::any::Any;

use axum::{
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use tower_http::{catch_panic::CatchPanicLayer, trace::TraceLayer};

use crate::{error::ApiError, state::AppState};

/// Build the whole application.
pub fn router(state: AppState) -> Router {
    // Unmatched `/v1/...` paths land here, behind the guard: an unauthenticated
    // caller gets 401 and learns nothing about which routes exist.
    let protected = Router::new()
        // P2+ mounts /me, /mailboxes, /threads, /search, /agents, /feed, … here.
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_bearer,
        ))
        .with_state(state.clone());

    let v1 = Router::new()
        .route("/healthz", get(health::healthz))
        .route("/auth/pair", post(auth::pair))
        // P3 adds /webhooks/gmail here - public, but OIDC-verified.
        .with_state(state)
        .fallback_service(protected);

    Router::new()
        .nest("/v1", v1)
        .fallback(not_found)
        // Inside the trace layer, so a caught panic is still logged as a 500.
        .layer(CatchPanicLayer::custom(panic_response))
        .layer(TraceLayer::new_for_http())
}

/// Anything we do not serve, in the standard envelope.
async fn not_found() -> ApiError {
    ApiError::not_found()
}

/// A panicking handler becomes a 500 envelope rather than a dropped connection.
pub(crate) fn panic_response(payload: Box<dyn Any + Send + 'static>) -> Response {
    let detail = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&'static str>()
                .map(|s| (*s).to_owned())
        })
        .unwrap_or_else(|| "<non-string panic payload>".to_owned());
    tracing::error!(%detail, "request handler panicked");
    ApiError::of(crate::error::ErrorCode::Internal).into_response()
}

#[cfg(test)]
mod tests {
    use axum::{http::StatusCode, routing::get};
    use tower_http::catch_panic::CatchPanicLayer;

    use super::*;
    use crate::{
        config::Env,
        test_support::{fixture, get as authed_get, response_json, send, test_app},
    };

    /// Criterion F8 - the guard covers routes that do not exist yet.
    #[tokio::test]
    async fn unknown_v1_routes_are_auth_guarded() {
        let app = test_app(Env::Prod).await;
        for path in [
            "/v1",
            "/v1/me",
            "/v1/mailboxes",
            "/v1/threads/abc",
            "/v1/feed",
            "/v1/agents/00000000-0000-0000-0000-000000000000",
            "/v1/healthz/extra",
            "/v1/auth",
            "/v1/auth/pair/extra",
        ] {
            let response = authed_get(&app, path, None).await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
            assert_eq!(
                response_json(response).await,
                fixture("error_unauthorized.json"),
                "{path}"
            );
        }

        // `/v1/` is the one shape the nest cannot match (matchit's `{*rest}`
        // needs at least one character), so it falls to the outer 404. It can
        // never be a real route, and the envelope leaks nothing either way.
        assert_eq!(
            authed_get(&app, "/v1/", None).await.status(),
            StatusCode::NOT_FOUND
        );
    }

    /// Criterion F9 - and the guard lets a real token through.
    #[tokio::test]
    async fn a_valid_token_reaches_the_router() {
        let app = test_app(Env::Prod).await;
        let code = app.state.pairing.mint().await.unwrap();
        let paired = response_json(
            crate::test_support::post_json(
                &app,
                "/v1/auth/pair",
                &serde_json::json!({ "code": code.code, "device_name": "jatin-iphone" }),
            )
            .await,
        )
        .await;
        let token = paired["token"].as_str().unwrap().to_owned();

        let response = authed_get(&app, "/v1/me", Some(&token)).await;
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "P1 has no /me yet, but the request must get past the guard"
        );
        assert_eq!(response_json(response).await["error"]["code"], "not_found");
    }

    #[tokio::test]
    async fn paths_outside_v1_are_plain_not_found() {
        let app = test_app(Env::Prod).await;
        let response = authed_get(&app, "/", None).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response_json(response).await["error"]["code"], "not_found");
    }

    /// Criterion G4.
    #[tokio::test]
    async fn a_panicking_handler_returns_the_internal_envelope() {
        let router = Router::new()
            .route("/boom", get(|| async { panic!("kaboom") as () }))
            .layer(CatchPanicLayer::custom(panic_response));

        let response = send(&router, "GET", "/boom", None, None).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = response_json(response).await;
        assert_eq!(body["error"]["code"], "internal");
        assert!(
            !body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("kaboom"),
            "the panic message must not reach the client"
        );
    }
}
