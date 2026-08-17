//! `GET /v1/auth/gmail/start` and `GET /v1/auth/gmail/callback`.
//!
//! **Both are browser-facing and therefore unauthenticated**, which is a
//! deliberate departure from `API.md` §0's exception list - see
//! backend/DECISIONS.md D15. `start` is a URL a human types into Safari, and
//! `callback` is where Google redirects; neither can carry a bearer token.
//!
//! Nothing is bound without a `state` we minted **and** the PKCE verifier that
//! goes with it, and a completed consent for a *different* mailbox is refused
//! rather than allowed to repoint the account.

use axum::{
    extract::{RawQuery, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
};

use crate::{
    gmail::oauth::{self, TokenError},
    state::AppState,
};

/// `GET /v1/auth/gmail/start` → 302 to Google, carrying PKCE + `state`.
pub async fn start(State(state): State<AppState>) -> Response {
    let Some(oauth_config) = state.gmail.oauth.clone() else {
        return page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Gmail is not configured",
            "This server has no <code>secrets/web_client.json</code>, so it cannot start a \
             Google sign-in. Add the OAuth client file and restart.",
        );
    };

    let (url, csrf_state, verifier) = oauth_config.authorize_url();
    state.gmail.pending.remember(csrf_state, verifier);

    let mut headers = HeaderMap::new();
    match axum::http::HeaderValue::from_str(&url) {
        Ok(value) => {
            headers.insert(header::LOCATION, value);
            // A consent URL is single-use and carries a `state`; nothing may
            // cache it.
            headers.insert(
                header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-store"),
            );
            (StatusCode::FOUND, headers).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "the Google consent URL was not a header value");
            page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Something went wrong",
                "The sign-in link could not be built. Check the server log.",
            )
        }
    }
}

/// `GET /v1/auth/gmail/callback` - browser-facing, so every outcome is a page a
/// person can read rather than a JSON envelope.
pub async fn callback(State(state): State<AppState>, RawQuery(query): RawQuery) -> Response {
    let query = query.unwrap_or_default();

    let (csrf_state, code) = match oauth::callback_params(&query) {
        Ok(pair) => pair,
        Err(error) => {
            tracing::warn!(error = %format!("{error:#}"), "gmail callback rejected");
            return page(
                StatusCode::BAD_REQUEST,
                "Sign-in did not complete",
                "Google did not send back what we expected. Start again from Settings.",
            );
        }
    };

    // Both halves, both verified: `state` proves the callback belongs to a flow
    // we started, and the PKCE verifier proves it belongs to *this* browser.
    // A replayed or expired `state` is indistinguishable from an unknown one.
    let Some(verifier) = state.gmail.pending.take(&csrf_state) else {
        tracing::warn!("gmail callback presented an unknown, spent, or expired state");
        return page(
            StatusCode::BAD_REQUEST,
            "That sign-in link has expired",
            "Start again from Settings - the link is single-use and lasts ten minutes.",
        );
    };

    let tokens = match state.gmail.tokens.exchange_code(&code, &verifier).await {
        Ok(tokens) => tokens,
        Err(TokenError::NotConnected) => {
            return page(
                StatusCode::SERVICE_UNAVAILABLE,
                "Gmail is not configured",
                "This server has no OAuth client file.",
            )
        }
        Err(error) => {
            tracing::warn!(%error, "the Gmail token exchange failed");
            return page(
                StatusCode::BAD_GATEWAY,
                "Google refused the sign-in",
                "The authorization code could not be exchanged. Try again from Settings.",
            );
        }
    };

    // Ask Gmail who just consented, rather than trusting anything in the
    // redirect. This is also the first real call, so a scope problem surfaces
    // here rather than during the first sync.
    let probe = state.gmail.probe_client(&tokens.access_token);
    let email = match probe.get_profile().await {
        Ok(profile) => profile.email_address,
        Err(error) => {
            tracing::warn!(%error, "could not read the Gmail profile after consent");
            return page(
                StatusCode::BAD_GATEWAY,
                "Google refused the sign-in",
                "We could not read your address from Gmail. Try again from Settings.",
            );
        }
    };

    // v1 is single-account. Binding a second mailbox would silently repoint
    // every stored message at a new owner, so refuse instead.
    match oauth::existing_account_email(&state.pool).await {
        Ok(Some(existing)) if !existing.eq_ignore_ascii_case(&email) => {
            tracing::warn!(%existing, attempted = %email, "refused to bind a second Gmail account");
            return page(
                StatusCode::CONFLICT,
                "This server is already connected",
                "It is signed in as a different Google account. v1 serves one mailbox; \
                 nothing was changed.",
            );
        }
        Ok(_) => {}
        Err(error) => {
            tracing::error!(%error, "could not read the existing account");
            return page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Something went wrong",
                "The account could not be read. Check the server log.",
            );
        }
    }

    match state.gmail.tokens.save_consent(&email, &tokens).await {
        Ok(account_id) => {
            tracing::info!(%account_id, %email, "gmail account connected");
            // A first sync, straight away, on the job queue.
            let queue = crate::jobs::Queue::new(state.pool.clone(), state.config.jobs.clone());
            if let Err(error) = crate::sync::enqueue(&queue, account_id).await {
                tracing::warn!(%error, "could not enqueue the first sync");
            }
            page(
                StatusCode::OK,
                "NADE is connected",
                &format!(
                    "Signed in as <strong>{}</strong>. Your mail is syncing now. \
                     You can close this tab.",
                    escape(&email)
                ),
            )
        }
        Err(error) => {
            tracing::error!(%error, "could not save the Gmail consent");
            page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Something went wrong",
                "The sign-in worked but could not be saved. Check the server log.",
            )
        }
    }
}

/// A plain page. No JavaScript, no external anything - this renders in whatever
/// browser the consent screen happened to be in.
fn page(status: StatusCode, title: &str, body: &str) -> Response {
    let html = format!(
        "<!doctype html>\n\
         <html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{title} — NADE</title>\
         <style>body{{font:16px/1.5 -apple-system,BlinkMacSystemFont,Segoe UI,sans-serif;\
         margin:0;display:grid;place-items:center;min-height:100vh;background:#faf8f4;color:#2b2723}}\
         main{{max-width:32rem;padding:2rem;text-align:center}}\
         h1{{font-size:1.35rem;margin:0 0 .75rem}}p{{margin:0;color:#5b534b}}\
         code{{background:#efeae1;padding:.1em .35em;border-radius:.25em}}</style>\
         </head><body><main><h1>{title}</h1><p>{body}</p></main></body></html>"
    );
    let mut response = (status, Html(html)).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

/// The email address is the only thing that reaches the page from outside, and
/// it comes from Gmail rather than from the request - but escaping it costs
/// nothing and removes the question.
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests;
