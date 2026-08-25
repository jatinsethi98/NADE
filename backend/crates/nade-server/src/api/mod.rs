//! The `/v1` router.
//!
//! Four public routes, and then *everything else* under `/v1` - including paths
//! that do not exist yet - goes through the bearer guard. That ordering is
//! deliberate: a route added in P2 is protected the moment it is written, and
//! forgetting to guard it is not a thing that can happen.
//!
//! The two browser-facing OAuth routes take no bearer header, but they are not
//! open either: `start` demands a single-use capability minted by the
//! authenticated `POST /auth/gmail/link`, and `callback` demands the cookie
//! `start` set. backend/DECISIONS.md D15.

pub mod agents;
pub mod auth;
pub mod cursor;
pub mod drafts;
pub mod feed;
pub mod gmail_auth;
pub mod health;
pub mod mail;
pub mod notes;
pub mod runs;
pub mod webhooks;

/// Our real response types, serialised and compared against `docs/contract/`.
#[cfg(test)]
mod contract_tests;

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
        .route("/me", get(mail::me))
        // Authorises the browser-facing `start` below. Behind the guard, which
        // is the entire point: only an already-paired device may hand out a
        // permission slip to link this server's mailbox.
        .route("/auth/gmail/link", post(gmail_auth::link))
        .route("/mailboxes", get(mail::mailboxes))
        .route("/mailboxes/{id}/threads", get(mail::threads))
        .route("/threads/{id}", get(mail::thread))
        .route("/search", get(mail::search))
        .route(
            "/messages/{gmail_id}/attachments/{att_id}",
            get(mail::attachment),
        )
        // P5. P7 mounts /settings.
        .route("/feed", get(feed::list))
        .route("/feed/seen", post(feed::seen))
        // `/feed/seen` is declared **before** `/feed/{id}`: axum's matcher
        // prefers a literal segment over a capture, so the order is not what
        // makes this correct — but reading them in this order is what makes it
        // obvious that "seen" is not a feed item id.
        .route("/feed/{id}", get(feed::detail))
        .route("/feed/{id}/approve", post(feed::approve))
        .route("/feed/{id}/skip", post(feed::skip))
        .route("/agents", get(agents::list).post(agents::create))
        .route(
            "/agents/{id}",
            get(agents::detail)
                .patch(agents::patch)
                .delete(agents::delete),
        )
        .route("/agents/{id}/run", post(agents::run_now))
        .route("/runs", get(runs::list))
        .route("/runs/{id}", get(runs::detail))
        .route("/notes", get(notes::list))
        .route("/notes/{id}", get(notes::detail))
        .route("/drafts", get(drafts::list))
        .route("/drafts/{id}", axum::routing::patch(drafts::patch))
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_bearer,
        ))
        .with_state(state.clone());

    let v1 = Router::new()
        .route("/healthz", get(health::healthz))
        .route("/auth/pair", post(auth::pair))
        // Browser-facing, so no bearer header - but not unauthorised:
        // `start` consumes a capability from `/auth/gmail/link`, and `callback`
        // requires the cookie `start` set. backend/DECISIONS.md D15.
        .route("/auth/gmail/start", get(gmail_auth::start))
        .route("/auth/gmail/callback", get(gmail_auth::callback))
        // Public, but not open: Pub/Sub cannot present a bearer, so the OIDC
        // token is verified in full instead. The method fallback matters -
        // without it a `GET` here answers `405` and confirms the route exists,
        // while every other unknown /v1 path answers `401`.
        .route(
            "/webhooks/gmail",
            post(webhooks::gmail)
                .fallback(|| async { crate::error::ApiError::unauthorized() })
                // `API.md` §0 caps a request body at 1 MB. axum's own default
                // is 2 MiB, so without this an unauthenticated caller could
                // force twice the buffering the contract promises - before
                // verification, because the body is read as `Bytes`.
                .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024)),
        )
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
            // The webhook is public, but only at exactly this path and exactly
            // this method. Everything around it must look like any other
            // unknown route.
            "/v1/webhooks",
            "/v1/webhooks/gmail/extra",
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

        // A route P2 serves: the guard lets it through.
        let response = authed_get(&app, "/v1/me", Some(&token)).await;
        assert_eq!(response.status(), StatusCode::OK);

        // And a route no phase serves yet reaches the *router's* 404 rather
        // than the guard's 401, which is what proves the middleware passes
        // through rather than short-circuiting. `/v1/agents` was this probe
        // until P4 mounted it and `/v1/feed` until P5; `/v1/settings` is P7's.
        let response = authed_get(&app, "/v1/settings", Some(&token)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
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
    /// Criterion T15 - a `GET` to the webhook answers exactly like an unknown
    /// route. Without the method fallback, axum answers `405`, which tells a
    /// prober the path exists and is worth attacking.
    #[tokio::test]
    async fn a_get_to_the_webhook_is_indistinguishable_from_an_unknown_route() {
        let app = test_app(Env::Prod).await;

        let probe = authed_get(&app, "/v1/webhooks/gmail", None).await;
        assert_eq!(probe.status(), StatusCode::UNAUTHORIZED);
        let probe_body = response_json(probe).await;

        let unknown = authed_get(&app, "/v1/no-such-route", None).await;
        assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(probe_body, response_json(unknown).await);
        assert_eq!(probe_body, fixture("error_unauthorized.json"));
    }
}
