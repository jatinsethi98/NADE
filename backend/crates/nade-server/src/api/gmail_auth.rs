//! Linking a Gmail account: one authenticated route and two browser-facing ones.
//!
//! | Route | Auth |
//! |---|---|
//! | `POST /v1/auth/gmail/link` | bearer, like everything else |
//! | `GET /v1/auth/gmail/start` | a single-use **capability** in the query |
//! | `GET /v1/auth/gmail/callback` | the `state` + PKCE + a binding **cookie** |
//!
//! The two browser routes still carry no bearer token - a URL typed into Safari
//! and a redirect from Google cannot set a header - but "carries no header" is
//! not "anyone may call it". `start` used to be reachable by anybody: on a
//! server that was up but not yet connected, a stranger could complete consent
//! with **their** Google account, become the singleton mailbox, and leave the
//! real owner staring at the 409. PKCE and `state` never addressed that; they
//! prove the callback belongs to a flow *someone* started, not that the someone
//! was allowed to configure this server.
//!
//! So authorisation to *initiate* now comes from an already-paired device
//! ([`link`] mints a capability behind the bearer guard), and the browser itself
//! is bound with an `HttpOnly` cookie that `callback` insists on. See
//! backend/DECISIONS.md D15.

pub mod capability;

use axum::{
    extract::{RawQuery, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    Json,
};
use chrono::{SecondsFormat, Utc};
use serde::Serialize;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::{
    api::auth::Auth,
    error::{ApiError, ApiResult},
    gmail::oauth::{self, TokenError, STATE_TTL},
    state::AppState,
};
use capability::CAPABILITY_TTL_SECS;

/// The cookie that binds a consent to the browser that started it.
///
/// `SameSite=Lax` is load-bearing rather than a default: Google's redirect back
/// is a **cross-site top-level GET navigation**, which `Lax` allows and `Strict`
/// would silently drop - turning every real sign-in into the refusal page.
const BINDING_COOKIE: &str = "nade_gmail_link";

// ----------------------------------------------------------------- minting --

/// `POST /v1/auth/gmail/link` → the URL to open in a browser.
///
/// `docs/contract/gmail_link.json`.
#[derive(Debug, Serialize)]
pub struct LinkResponse {
    /// Absolute, single-use, and dead in ten minutes.
    ///
    /// It carries the capability in the query string, which is exactly why the
    /// capability is single-use and short-lived: a URL ends up in shell history,
    /// browser history and access logs, so it has to be worthless by the time it
    /// gets there. It is consumed *before* the redirect to Google, so it never
    /// leaves this origin.
    pub url: String,
    /// ISO-8601 UTC, second precision, `Z`-suffixed (`API.md` §0).
    ///
    /// A display value, computed from the wall clock. Enforcement is monotonic
    /// (`Instant`), so a clock jump can make this figure wrong without making
    /// the TTL wrong.
    pub expires_at: String,
}

/// `POST /v1/auth/gmail/link` - **authenticated**, and the only way to obtain a
/// usable `start` URL.
///
/// Pairing is the trust root and there is no bootstrapping problem: the pairing
/// code is printed on the server's own console, so a device holding a bearer
/// token has already proved console access. A caller who cannot reach the
/// console cannot reach this route, and therefore cannot reach `start` either.
///
/// # Errors
/// `401` without a bearer (the guard, not this handler); `500` if the OS
/// randomness source or the audit write fails.
pub async fn link(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<LinkResponse>> {
    // The dev-token principal is synthetic and has no `devices` row, so there is
    // nothing to revoke later; record `None` rather than a uuid that will never
    // resolve.
    let device_id = (!auth.device.is_dev_token()).then_some(auth.device.id);

    let granted = state
        .gmail_link
        .mint(device_id)
        .map_err(|error| ApiError::internal("minting a Gmail link capability", &error))?;

    sqlx::query(
        "insert into audit_log (account_id, actor, action, subject) \
         values ($1, $2, 'gmail_link_requested', $3)",
    )
    .bind(auth.account.as_ref().map(|account| account.id))
    .bind(format!("device:{}", auth.device.id))
    .bind(serde_json::json!({ "device_name": auth.device.device_name }))
    .execute(&state.pool)
    .await?;

    tracing::info!(device = %auth.device.id, "minted a Gmail link capability");
    Ok(Json(LinkResponse {
        url: start_url(&state.config.gmail.redirect_uri, &granted),
        expires_at: (Utc::now() + chrono::TimeDelta::seconds(ttl_seconds()))
            .to_rfc3339_opts(SecondsFormat::Secs, true),
    }))
}

/// [`CAPABILITY_TTL_SECS`] as the signed seconds `chrono` wants. A `const` this
/// small cannot overflow, and the cast is checked here once rather than at the
/// call site.
const fn ttl_seconds() -> i64 {
    #[allow(clippy::cast_possible_wrap)]
    {
        CAPABILITY_TTL_SECS as i64
    }
}

// ------------------------------------------------------------------- start --

/// `GET /v1/auth/gmail/start?cap=…` → 302 to Google, carrying PKCE + `state`,
/// and setting the binding cookie.
///
/// Without a live capability this renders a refusal page. It is deliberately
/// **not** a redirect, and deliberately says nothing about whether a mailbox is
/// already connected: a prober must not be able to use this route to find out
/// whether a NADE server is up for grabs.
pub async fn start(State(state): State<AppState>, RawQuery(query): RawQuery) -> Response {
    let presented = query
        .as_deref()
        .and_then(|query| oauth::query_param(query, "cap"));

    // EDGE (absent / malformed / expired / replayed capability): `take` folds
    // all four into `None`, and they share one page, so the refusal is not an
    // oracle for which of them happened.
    let Some(granted) = presented.and_then(|cap| state.gmail_link.take(&cap)) else {
        tracing::warn!("gmail start was called without a live capability");
        return refusal();
    };

    // EDGE (a capability minted by a device that has since been revoked):
    // revoking a device revokes what it authorised. `callback` checks the same
    // thing again, so revoking also closes a consent that is already half way
    // through Google rather than only refusing new ones.
    if let Some(device_id) = granted.device_id {
        match device_is_live(&state.pool, device_id).await {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!(device = %device_id, "gmail start refused: the device is revoked");
                return refusal();
            }
            Err(error) => {
                tracing::error!(%error, "could not check the linking device");
                return page(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Something went wrong",
                    "The sign-in could not be started. Check the server log.",
                );
            }
        }
    }

    // Checked after the capability, so an unauthorised caller cannot use this
    // route to fingerprint the server's configuration either.
    let Some(oauth_config) = state.gmail.oauth.clone() else {
        return page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Gmail is not configured",
            "This server has no <code>secrets/web_client.json</code>, so it cannot start a \
             Google sign-in. Add the OAuth client file and restart.",
        );
    };

    let binding = match capability::random_secret() {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "could not mint the browser binding");
            return page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Something went wrong",
                "The sign-in link could not be built. Check the server log.",
            );
        }
    };

    let (url, csrf_state, verifier) = oauth_config.authorize_url();

    // Both headers are built *before* the flow is remembered, so a response we
    // cannot send leaves no pending consent behind to age out.
    let Ok(location) = axum::http::HeaderValue::from_str(&url) else {
        tracing::error!(%url, "the Google consent URL was not a header value");
        return page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Something went wrong",
            "The sign-in link could not be built. Check the server log.",
        );
    };
    let Ok(cookie) = axum::http::HeaderValue::from_str(&binding_cookie(
        &binding,
        &state.config.gmail.redirect_uri,
    )) else {
        tracing::error!("the binding cookie was not a header value");
        return page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Something went wrong",
            "The sign-in link could not be built. Check the server log.",
        );
    };

    // EDGE (two `start` calls interleaved): each gets its own `state` and its
    // own cookie value. Two browsers can both finish; one browser keeps only
    // the newest cookie, so its earlier `state` becomes unredeemable and ages
    // out - which is correct, not a leak.
    state.gmail.pending.remember(
        csrf_state,
        oauth::Pending {
            verifier,
            binding,
            device_id: granted.device_id,
        },
    );

    let mut headers = HeaderMap::new();
    headers.insert(header::LOCATION, location);
    headers.insert(header::SET_COOKIE, cookie);
    // A consent URL is single-use and carries a `state`; nothing may cache it.
    headers.insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    (StatusCode::FOUND, headers).into_response()
}

/// Is this device still paired?
async fn device_is_live(pool: &sqlx::PgPool, device_id: Uuid) -> sqlx::Result<bool> {
    let live: Option<i32> =
        sqlx::query_scalar("select 1 from devices where id = $1 and revoked_at is null")
            .bind(device_id)
            .fetch_optional(pool)
            .await?;
    Ok(live.is_some())
}

// ---------------------------------------------------------------- callback --

/// `GET /v1/auth/gmail/callback` - browser-facing, so every outcome is a page a
/// person can read rather than a JSON envelope.
pub async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
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

    // All three halves, all three verified: `state` proves the callback belongs
    // to a flow we started, the PKCE verifier proves the code was minted for it,
    // and the cookie proves this is the same browser that started it. A
    // replayed, expired or unknown `state` is indistinguishable from the rest.
    let Some(pending) = state.gmail.pending.take(&csrf_state) else {
        tracing::warn!("gmail callback presented an unknown, spent, or expired state");
        return expired_link();
    };

    // EDGE (cookie absent / mismatched / belonging to a *different* pending
    // state): refused **before** the token exchange, so a lifted `state` cannot
    // even spend the authorization code. The `state` is gone either way, which
    // caps a cookie-guessing attempt at one try per consent.
    if !binding_matches(&headers, &pending.binding) {
        tracing::warn!("gmail callback did not carry the binding cookie for its state");
        return expired_link();
    }

    // EDGE (a callback racing a device revoke): the device is checked again
    // here, not only at `start`. Revoking a stolen token has to close a consent
    // that is already half way through Google, or the thief simply finishes it
    // and binds *their* mailbox to this server.
    if let Some(device_id) = pending.device_id {
        match device_is_live(&state.pool, device_id).await {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!(device = %device_id, "gmail callback refused: the device is revoked");
                return expired_link();
            }
            Err(error) => {
                tracing::error!(%error, "could not check the linking device");
                return page(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Something went wrong",
                    "The sign-in could not be completed. Check the server log.",
                );
            }
        }
    }

    let tokens = match state
        .gmail
        .tokens
        .exchange_code(&code, &pending.verifier)
        .await
    {
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

    // v1 is one mailbox per device. Rebinding this device to a second mailbox
    // would silently repoint its whole view of its mail at a new owner, so
    // refuse instead. A different, unbound device consenting a new mailbox is
    // not this case - that is a second user, and it simply proceeds.
    //
    // This is the **cheap pre-check**: it buys the friendly page without a
    // write. The authoritative one lives inside `save_consent`, under the
    // account advisory lock, because two callbacks can reach this line at the
    // same moment and both see "unbound".
    if let Some(device_id) = pending.device_id {
        match oauth::bound_account_email(&state.pool, device_id).await {
            Ok(Some(existing)) if !existing.eq_ignore_ascii_case(&email) => {
                tracing::warn!(%existing, attempted = %email, "refused to rebind a device to a second Gmail account");
                return already_connected();
            }
            Ok(_) => {}
            Err(error) => {
                tracing::error!(%error, "could not read the device's bound account");
                return page(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Something went wrong",
                    "The account could not be read. Check the server log.",
                );
            }
        }
    }

    match state
        .gmail
        .tokens
        .save_consent(&email, &tokens, pending.device_id)
        .await
    {
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
        // The race the pre-check above cannot win: another callback bound this
        // device first. Same page, because it is the same fact.
        Err(TokenError::AlreadyBound { existing }) => {
            tracing::warn!(%existing, attempted = %email, "lost the race to bind the device");
            already_connected()
        }
        // A revoke landed between the pre-exchange check and the commit. Same
        // page as the pre-exchange refusal, so the revocation's timing is not
        // an oracle - and nothing was persisted, because the bind aborts the
        // whole consent transaction.
        Err(TokenError::DeviceRevoked) => {
            tracing::warn!("gmail callback refused at commit: the device is revoked");
            expired_link()
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

// -------------------------------------------------------------- the pieces --

/// The one page every unauthorised `start` gets.
///
/// It names no reason and no account, and it is byte-identical whether or not
/// this server already has a mailbox - the whole point of moving authorisation
/// to a capability is undone if the refusal itself answers "is this server up
/// for grabs?".
fn refusal() -> Response {
    page(
        StatusCode::FORBIDDEN,
        "Start from the NADE app",
        "Open Settings in NADE and choose to connect Gmail. That link is single-use and \
         lasts ten minutes.",
    )
}

/// `state` unknown, spent, expired, or in the wrong browser. One page for all
/// four, for the same reason as [`refusal`].
fn expired_link() -> Response {
    page(
        StatusCode::BAD_REQUEST,
        "That sign-in link has expired",
        "Start again from Settings - the link is single-use, lasts ten minutes, and has to \
         finish in the browser that opened it.",
    )
}

fn already_connected() -> Response {
    page(
        StatusCode::CONFLICT,
        "This device is already connected",
        "It is signed in as a different Google account. v1 serves one mailbox per device; \
         nothing was changed.",
    )
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

/// The `Set-Cookie` value `start` emits.
///
/// * `HttpOnly` - no page script may read it, so an XSS anywhere on the origin
///   cannot lift a live consent.
/// * `SameSite=Lax` - see [`BINDING_COOKIE`]; `Strict` would break the callback.
/// * `Secure` only when the registered redirect is `https`, because a `Secure`
///   cookie is simply dropped over plain `http` and the dev server is
///   `http://localhost`.
/// * `Path` scoped to the OAuth routes, derived from the registered redirect URI
///   rather than assumed, so a server behind a path-rewriting proxy still works.
fn binding_cookie(value: &str, redirect_uri: &str) -> String {
    let secure = if redirect_uri.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    format!(
        "{BINDING_COOKIE}={value}; Max-Age={}; Path={}; HttpOnly; SameSite=Lax{secure}",
        STATE_TTL.as_secs(),
        cookie_path(redirect_uri),
    )
}

/// Constant-time, across every `Cookie` header and every cookie inside each.
///
/// EDGE (unicode): a `Cookie` header carrying bytes that are not header text
/// fails `to_str` and is treated as absent - never a panic, never a 500.
/// EDGE (duplicate names): a browser may legitimately send two cookies of the
/// same name from different paths, so every candidate is compared rather than
/// only the first.
fn binding_matches(headers: &HeaderMap, expected: &str) -> bool {
    if expected.is_empty() {
        return false;
    }
    let mut matched = false;
    for header in headers.get_all(header::COOKIE) {
        let Ok(raw) = header.to_str() else { continue };
        for pair in raw.split(';') {
            let Some((name, value)) = pair.split_once('=') else {
                continue;
            };
            if name.trim() != BINDING_COOKIE {
                continue;
            }
            // No early exit: the answer must not depend on which cookie matched.
            if bool::from(value.trim().as_bytes().ct_eq(expected.as_bytes())) {
                matched = true;
            }
        }
    }
    matched
}

/// The URL a paired device hands to the browser.
///
/// Derived from the **registered redirect URI**, because that - not `NADE_BIND`,
/// which may be `0.0.0.0` or a container-local address - is the origin the
/// browser and Google can both reach.
fn start_url(redirect_uri: &str, granted: &str) -> String {
    let base = match redirect_uri.strip_suffix("/callback") {
        Some(prefix) => format!("{prefix}/start"),
        // A redirect URI that does not end in `/callback` can only have come
        // from a hand-set `NADE_GMAIL_REDIRECT_URI`; fall back to this server's
        // own route on the same origin rather than inventing a path.
        None => format!("{}/v1/auth/gmail/start", origin(redirect_uri)),
    };
    // The capability is 64 hex characters, so there is nothing to encode.
    format!("{base}?cap={granted}")
}

fn origin(uri: &str) -> String {
    let (scheme, rest) = uri.split_once("://").unwrap_or(("http", uri));
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    format!("{scheme}://{authority}")
}

/// The directory the redirect URI lives in: `/v1/auth/gmail/callback` →
/// `/v1/auth/gmail`, which covers `start` and `callback` and nothing else.
fn cookie_path(redirect_uri: &str) -> String {
    let path = uri_path(redirect_uri);
    match path.rfind('/') {
        Some(0) | None => "/".to_owned(),
        Some(cut) => path[..cut].to_owned(),
    }
}

fn uri_path(uri: &str) -> &str {
    let after_scheme = uri.split_once("://").map_or(uri, |(_, rest)| rest);
    match after_scheme.find('/') {
        Some(start) => after_scheme[start..]
            .split(['?', '#'])
            .next()
            .unwrap_or("/"),
        None => "/",
    }
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
