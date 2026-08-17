//! Gmail OAuth: consent, token exchange, and the refresh that must never lose a
//! rotated token.
//!
//! The prior art for this project lost its Gmail access by treating the refresh
//! token as a constant. Google rotates it: a refresh response *may* carry a new
//! `refresh_token`, and if you keep using the old one you get `invalid_grant`
//! forever. So the rule here is absolute - **every refresh writes back**, and a
//! refresh that returns no new token keeps the old one rather than nulling it.

use std::{
    collections::HashMap,
    path::Path,
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use oauth2::{
    basic::{BasicClient, BasicErrorResponseType},
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointNotSet, EndpointSet,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, RefreshToken, RequestTokenError, Scope,
    TokenResponse as _, TokenUrl,
};
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

use super::crypto::Cipher;

/// Read-only Gmail. The smallest scope that serves v1, which takes no outbound
/// action of any kind.
pub const SCOPE: &str = "https://www.googleapis.com/auth/gmail.readonly";

/// How long a consent round trip may take before its `state` is refused.
pub const STATE_TTL: Duration = Duration::from_secs(600);

/// Beyond this many pending consents, the oldest are dropped. An unauthenticated
/// browser route must not be a memory-growth lever.
const MAX_PENDING: usize = 64;

/// Refresh this long before the access token actually dies.
///
/// EDGE (clock skew): our clock and Google's disagree, and a token that expires
/// mid-request produces a 401 in the middle of a batch. A minute of margin costs
/// nothing and removes the whole class.
pub const EXPIRY_SKEW: chrono::TimeDelta = chrono::TimeDelta::seconds(60);

// ------------------------------------------------------------- the client --

type ConfiguredClient =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

/// Everything from `secrets/web_client.json`, plus the redirect we registered.
#[derive(Clone)]
pub struct OAuthConfig {
    client: ConfiguredClient,
    pub redirect_uri: String,
    pub auth_url: String,
    pub token_url: String,
}

impl std::fmt::Debug for OAuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthConfig")
            .field("redirect_uri", &self.redirect_uri)
            .field("auth_url", &self.auth_url)
            .field("token_url", &self.token_url)
            .finish_non_exhaustive()
    }
}

/// Google's downloaded client JSON.
#[derive(Debug, Deserialize)]
struct ClientFile {
    web: WebClient,
}

#[derive(Debug, Deserialize)]
struct WebClient {
    client_id: String,
    client_secret: String,
    #[serde(default)]
    auth_uri: Option<String>,
    #[serde(default)]
    token_uri: Option<String>,
    #[serde(default)]
    redirect_uris: Vec<String>,
}

impl OAuthConfig {
    /// Load `secrets/web_client.json`.
    ///
    /// `auth_override`/`token_override` exist so the wiremock suite can point the
    /// whole flow at a local server; production leaves them unset and uses
    /// Google's own URLs from the file.
    ///
    /// # Errors
    /// Returns an error if the file is missing, malformed, or the URLs are not
    /// URLs.
    pub fn load(
        path: &Path,
        redirect_uri: Option<&str>,
        auth_override: Option<&str>,
        token_override: Option<&str>,
    ) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading the Gmail client file {}", path.display()))?;
        let file: ClientFile = serde_json::from_str(&text)
            .with_context(|| format!("{} is not a Google web client file", path.display()))?;
        Self::from_parts(
            &file.web.client_id,
            &file.web.client_secret,
            redirect_uri
                .or_else(|| file.web.redirect_uris.first().map(String::as_str))
                .unwrap_or("http://localhost:8080/v1/auth/gmail/callback"),
            auth_override.or(file.web.auth_uri.as_deref()).unwrap_or(
                "https://accounts.google.com/o/oauth2/auth",
            ),
            token_override
                .or(file.web.token_uri.as_deref())
                .unwrap_or("https://oauth2.googleapis.com/token"),
        )
    }

    /// # Errors
    /// Returns an error if either URL is malformed.
    pub fn from_parts(
        client_id: &str,
        client_secret: &str,
        redirect_uri: &str,
        auth_url: &str,
        token_url: &str,
    ) -> Result<Self> {
        let client = BasicClient::new(ClientId::new(client_id.to_owned()))
            .set_client_secret(ClientSecret::new(client_secret.to_owned()))
            .set_auth_uri(AuthUrl::new(auth_url.to_owned()).context("the OAuth auth URL")?)
            .set_token_uri(TokenUrl::new(token_url.to_owned()).context("the OAuth token URL")?)
            .set_redirect_uri(
                RedirectUrl::new(redirect_uri.to_owned()).context("the OAuth redirect URI")?,
            );
        Ok(Self {
            client,
            redirect_uri: redirect_uri.to_owned(),
            auth_url: auth_url.to_owned(),
            token_url: token_url.to_owned(),
        })
    }

    /// The consent URL, plus the `state` and PKCE verifier the callback must
    /// present.
    ///
    /// `access_type=offline` + `prompt=consent` is what makes Google hand over a
    /// refresh token at all; without both, a returning user gets an access token
    /// only and background sync dies at the first expiry.
    #[must_use]
    pub fn authorize_url(&self) -> (String, String, String) {
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, state) = self
            .client
            .authorize_url(CsrfToken::new_random)
            .add_scope(Scope::new(SCOPE.to_owned()))
            .add_extra_param("access_type", "offline")
            .add_extra_param("prompt", "consent")
            .add_extra_param("include_granted_scopes", "true")
            .set_pkce_challenge(challenge)
            .url();
        (
            url.to_string(),
            state.into_secret(),
            verifier.into_secret(),
        )
    }
}

// ------------------------------------------------------ pending consents --

/// `state` → PKCE verifier, single-use and expiring.
#[derive(Debug, Default)]
pub struct PendingAuths {
    entries: Mutex<HashMap<String, (String, Instant)>>,
}

impl PendingAuths {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn remember(&self, state: String, verifier: String) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        entries.retain(|_, (_, born)| now.duration_since(*born) < STATE_TTL);
        if entries.len() >= MAX_PENDING {
            // Drop the oldest rather than refusing a legitimate new consent.
            if let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, (_, born))| *born)
                .map(|(key, _)| key.clone())
            {
                entries.remove(&oldest);
            }
        }
        entries.insert(state, (verifier, now));
    }

    /// Consume a `state`. `None` when it is unknown, already spent, or expired -
    /// which are deliberately indistinguishable to the caller.
    pub fn take(&self, state: &str) -> Option<String> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let (verifier, born) = entries.remove(state)?;
        (Instant::now().duration_since(born) < STATE_TTL).then_some(verifier)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// --------------------------------------------------------------- tokens --

/// What a token endpoint gave us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreshTokens {
    pub access_token: String,
    /// `None` when the response carried no `refresh_token`, which is the normal
    /// case for a refresh that did not rotate.
    pub refresh_token: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub scopes: Vec<String>,
}

/// Why a token operation failed. `NeedsReauth` is the one the whole lifecycle
/// hangs off.
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    /// The refresh token is dead. Only a fresh trip through
    /// `/v1/auth/gmail/start` fixes it.
    #[error("the Gmail credential is no longer valid; the user must re-consent")]
    NeedsReauth,
    #[error("no Gmail account is connected")]
    NotConnected,
    #[error("gmail oauth: {0}")]
    Other(String),
}

impl From<anyhow::Error> for TokenError {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(format!("{error:#}"))
    }
}

impl From<sqlx::Error> for TokenError {
    fn from(error: sqlx::Error) -> Self {
        Self::Other(format!("{error}"))
    }
}

/// The source of a bearer token for the Gmail client. A trait so the wiremock
/// suite can inject a fixed token without a database.
#[async_trait]
pub trait AccessTokens: Send + Sync + std::fmt::Debug {
    async fn access_token(&self) -> Result<String, TokenError>;
    /// Called after a 401: the next `access_token()` must not reuse the cached
    /// value.
    async fn invalidate(&self);
}

/// A fixed token. Tests only.
#[derive(Debug)]
pub struct StaticTokens(pub String);

#[async_trait]
impl AccessTokens for StaticTokens {
    async fn access_token(&self) -> Result<String, TokenError> {
        Ok(self.0.clone())
    }
    async fn invalidate(&self) {}
}

/// The database-backed token store.
pub struct TokenStore {
    pool: PgPool,
    cipher: Cipher,
    oauth: Option<std::sync::Arc<OAuthConfig>>,
    http: reqwest::Client,
    /// Serialises refreshes. Two workers refreshing at once would both spend the
    /// same rotating refresh token, and the loser's copy would be dead.
    refresh_gate: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for TokenStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenStore")
            .field("configured", &self.oauth.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct StoredRow {
    access_token: Option<String>,
    access_expiry: Option<DateTime<Utc>>,
    refresh_token: Option<String>,
}

impl TokenStore {
    #[must_use]
    pub fn new(
        pool: PgPool,
        cipher: Cipher,
        oauth: Option<std::sync::Arc<OAuthConfig>>,
        http: reqwest::Client,
    ) -> Self {
        Self {
            pool,
            cipher,
            oauth,
            http,
            refresh_gate: tokio::sync::Mutex::new(()),
        }
    }

    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.oauth.is_some()
    }

    /// Exchange an authorization code. Used only by the callback.
    ///
    /// # Errors
    /// Returns an error if OAuth is unconfigured or Google refuses the code.
    pub async fn exchange_code(&self, code: &str, verifier: &str) -> Result<FreshTokens, TokenError> {
        let oauth = self.oauth.as_ref().ok_or(TokenError::NotConnected)?;
        let bridge = |request: oauth2::HttpRequest| send(&self.http, request);
        let response = oauth
            .client
            .exchange_code(AuthorizationCode::new(code.to_owned()))
            .set_pkce_verifier(PkceCodeVerifier::new(verifier.to_owned()))
            .request_async(&bridge)
            .await
            .map_err(classify)?;
        Ok(shape(response))
    }

    /// Persist a fresh consent, creating or updating the single account row.
    ///
    /// # Errors
    /// Returns an error if the write fails.
    pub async fn save_consent(&self, email: &str, tokens: &FreshTokens) -> Result<Uuid, TokenError> {
        let mut tx = self.pool.begin().await?;

        let account_id: Uuid = sqlx::query_scalar(
            "insert into accounts (email, status) values ($1, 'ok') \
             on conflict (email) do update set status = 'ok' returning id",
        )
        .bind(email)
        .fetch_one(&mut *tx)
        .await?;

        let access = self
            .cipher
            .encrypt(&tokens.access_token)
            .map_err(TokenError::from)?;
        let refresh = match &tokens.refresh_token {
            Some(value) => Some(self.cipher.encrypt(value).map_err(TokenError::from)?),
            None => None,
        };

        // `coalesce` on refresh: a re-consent that somehow returns no refresh
        // token must not wipe the one we already have.
        sqlx::query(
            "insert into gmail_tokens \
                 (account_id, access_token, access_expiry, refresh_token, scopes, updated_at) \
             values ($1, $2, $3, $4, $5, now()) \
             on conflict (account_id) do update set \
                 access_token = excluded.access_token, \
                 access_expiry = excluded.access_expiry, \
                 refresh_token = coalesce(excluded.refresh_token, gmail_tokens.refresh_token), \
                 scopes = excluded.scopes, \
                 updated_at = now()",
        )
        .bind(account_id)
        .bind(&access)
        .bind(tokens.expires_at)
        .bind(refresh.as_ref())
        .bind(&tokens.scopes)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "insert into audit_log (account_id, actor, action, subject) \
             values ($1, 'user', 'gmail_consent', $2)",
        )
        .bind(account_id)
        .bind(serde_json::json!({ "email": email, "scopes": tokens.scopes }))
        .execute(&mut *tx)
        .await?;

        // Re-consent clears a previous `needs_reauth` info card.
        sqlx::query(
            "update feed_items set status = 'resolved', resolved_note = 'Reconnected.' \
              where account_id = $1 and kind = 'info' and status = 'new' \
                and data->>'reason' = 'needs_reauth'",
        )
        .bind(account_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(account_id)
    }

    /// A usable access token for this account, refreshing if it is close to
    /// expiry.
    ///
    /// # Errors
    /// [`TokenError::NeedsReauth`] when the refresh token is dead.
    pub async fn access_token(&self, account_id: Uuid) -> Result<String, TokenError> {
        if let Some(live) = self.cached_access_token(account_id).await? {
            return Ok(live);
        }
        self.refresh(account_id).await
    }

    async fn cached_access_token(&self, account_id: Uuid) -> Result<Option<String>, TokenError> {
        let row = self.row(account_id).await?;
        let (Some(sealed), Some(expiry)) = (row.access_token, row.access_expiry) else {
            return Ok(None);
        };
        if expiry <= Utc::now() + EXPIRY_SKEW {
            return Ok(None);
        }
        Ok(self.cipher.decrypt(&sealed).ok())
    }

    async fn row(&self, account_id: Uuid) -> Result<StoredRow, TokenError> {
        sqlx::query_as::<_, StoredRow>(
            "select access_token, access_expiry, refresh_token from gmail_tokens \
              where account_id = $1",
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(TokenError::NotConnected)
    }

    /// Refresh, and **persist the result** - including a rotated refresh token.
    async fn refresh(&self, account_id: Uuid) -> Result<String, TokenError> {
        let oauth = self.oauth.as_ref().ok_or(TokenError::NotConnected)?;
        let _gate = self.refresh_gate.lock().await;

        // Somebody else may have refreshed while we waited for the gate.
        if let Some(live) = self.cached_access_token(account_id).await? {
            return Ok(live);
        }

        let row = self.row(account_id).await?;
        let sealed = row.refresh_token.ok_or(TokenError::NeedsReauth)?;
        let refresh_token = self
            .cipher
            .decrypt(&sealed)
            .map_err(|_| TokenError::NeedsReauth)?;

        let bridge = |request: oauth2::HttpRequest| send(&self.http, request);
        let response = oauth
            .client
            .exchange_refresh_token(&RefreshToken::new(refresh_token.clone()))
            .request_async(&bridge)
            .await;

        let tokens = match response {
            Ok(response) => shape(response),
            Err(error) => {
                let classified = classify(error);
                if matches!(classified, TokenError::NeedsReauth) {
                    self.mark_needs_reauth(account_id, "invalid_grant").await?;
                }
                return Err(classified);
            }
        };

        let access = self
            .cipher
            .encrypt(&tokens.access_token)
            .map_err(TokenError::from)?;
        // ROTATION: Google may hand back a new refresh token on any refresh.
        // Writing it back is the entire reason this code exists.
        let rotated = match &tokens.refresh_token {
            Some(new) if *new != refresh_token => {
                tracing::info!("gmail rotated the refresh token; persisting the new one");
                Some(self.cipher.encrypt(new).map_err(TokenError::from)?)
            }
            _ => None,
        };

        sqlx::query(
            "update gmail_tokens set \
                 access_token = $2, \
                 access_expiry = $3, \
                 refresh_token = coalesce($4, refresh_token), \
                 updated_at = now() \
              where account_id = $1",
        )
        .bind(account_id)
        .bind(&access)
        .bind(tokens.expires_at)
        .bind(rotated.as_ref())
        .execute(&self.pool)
        .await?;

        Ok(tokens.access_token)
    }

    /// Force the next call to refresh. Used after a 401 from Gmail.
    ///
    /// # Errors
    /// Returns an error if the write fails.
    pub async fn invalidate(&self, account_id: Uuid) -> Result<(), TokenError> {
        sqlx::query("update gmail_tokens set access_expiry = now() - interval '1 hour' where account_id = $1")
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// The `invalid_grant` lifecycle: the account goes `needs_reauth`, sync
    /// pauses, an `info` feed row appears, and an audit row records it.
    ///
    /// Idempotent: a second failure does not stack a second card on the feed.
    ///
    /// # Errors
    /// Returns an error if the transaction fails.
    pub async fn mark_needs_reauth(&self, account_id: Uuid, reason: &str) -> Result<(), TokenError> {
        let mut tx = self.pool.begin().await?;

        let changed = sqlx::query(
            "update accounts set status = 'needs_reauth' \
              where id = $1 and status <> 'needs_reauth'",
        )
        .bind(account_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();

        // EDGE (duplicate delivery): the feed row is written only when there is
        // not already an unresolved one, so repeated failures do not spam.
        sqlx::query(
            "insert into feed_items (account_id, kind, title, body, data, status) \
             select $1, 'info', 'Gmail', \
                    'NADE lost access to your Gmail. Open Settings and sign in again to resume sync.', \
                    $2::jsonb, 'new' \
              where not exists ( \
                  select 1 from feed_items \
                   where account_id = $1 and kind = 'info' and status = 'new' \
                     and data->>'reason' = 'needs_reauth')",
        )
        .bind(account_id)
        .bind(serde_json::json!({
            "action": "none",
            "reason": "needs_reauth",
            "note_id": serde_json::Value::Null,
            "thread_id": serde_json::Value::Null,
        }))
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "insert into audit_log (account_id, actor, action, subject) \
             values ($1, 'system', 'gmail_needs_reauth', $2)",
        )
        .bind(account_id)
        .bind(serde_json::json!({ "reason": reason, "first_time": changed > 0 }))
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        tracing::warn!(%account_id, reason, "gmail credential is dead; sync paused");
        Ok(())
    }
}

/// A [`TokenStore`] bound to one account, which is what the client wants.
#[derive(Debug)]
pub struct AccountTokens {
    pub store: std::sync::Arc<TokenStore>,
    pub account_id: Uuid,
}

#[async_trait]
impl AccessTokens for AccountTokens {
    async fn access_token(&self) -> Result<String, TokenError> {
        self.store.access_token(self.account_id).await
    }

    async fn invalidate(&self) {
        if let Err(error) = self.store.invalidate(self.account_id).await {
            tracing::warn!(%error, "could not invalidate the cached access token");
        }
    }
}

// -------------------------------------------------------- oauth2 plumbing --

fn shape(response: oauth2::basic::BasicTokenResponse) -> FreshTokens {
    FreshTokens {
        access_token: response.access_token().secret().clone(),
        refresh_token: response
            .refresh_token()
            .map(|token| token.secret().clone())
            .filter(|token| !token.is_empty()),
        expires_at: response.expires_in().and_then(|lifetime| {
            chrono::TimeDelta::from_std(lifetime)
                .ok()
                .map(|delta| Utc::now() + delta)
        }),
        scopes: response.scopes().map_or_else(
            || vec![SCOPE.to_owned()],
            |scopes| scopes.iter().map(|scope| scope.to_string()).collect(),
        ),
    }
}

/// `invalid_grant` is the one error with a lifecycle behind it; everything else
/// is a transient upstream problem.
fn classify<E: std::error::Error + 'static>(
    error: RequestTokenError<E, oauth2::basic::BasicErrorResponse>,
) -> TokenError {
    match &error {
        RequestTokenError::ServerResponse(response)
            if matches!(response.error(), BasicErrorResponseType::InvalidGrant) =>
        {
            TokenError::NeedsReauth
        }
        _ => TokenError::Other(format!("{error}")),
    }
}

/// Bridge `oauth2`'s `http::Request`/`http::Response` onto our `reqwest` client.
///
/// Doing it by hand is why `oauth2` is `default-features = false`: its own
/// `reqwest` feature would pull a second copy of reqwest and a second TLS stack
/// into the tree.
async fn send(
    http: &reqwest::Client,
    request: oauth2::HttpRequest,
) -> Result<oauth2::HttpResponse, BridgeError> {
    let (parts, body) = request.into_parts();
    let mut builder = http.request(parts.method, parts.uri.to_string());
    for (name, value) in &parts.headers {
        builder = builder.header(name, value);
    }
    let response = builder
        .body(body)
        .send()
        .await
        .map_err(|error| BridgeError(error.to_string()))?;

    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| BridgeError(error.to_string()))?;

    let mut out = http::Response::builder().status(status);
    for (name, value) in &headers {
        out = out.header(name, value);
    }
    out.body(bytes.to_vec())
        .map_err(|error| BridgeError(error.to_string()))
}

#[derive(Debug, thiserror::Error)]
#[error("http: {0}")]
pub struct BridgeError(String);

/// Guard against a second Gmail account quietly replacing the first.
///
/// v1 is single-account by design. If somebody completes consent for a different
/// mailbox, binding it would silently repoint every stored message at a new
/// owner. Refuse instead.
///
/// # Errors
/// Returns an error if the query fails.
pub async fn existing_account_email(pool: &PgPool) -> Result<Option<String>> {
    let email: Option<String> =
        sqlx::query_scalar("select email from accounts order by created_at limit 1")
            .fetch_optional(pool)
            .await?;
    Ok(email)
}

/// Read a `state` and `code` out of the callback query string.
///
/// # Errors
/// Returns an error naming what is missing, so the browser page can say it.
pub fn callback_params(query: &str) -> Result<(String, String)> {
    let mut state = None;
    let mut code = None;
    let mut denied = None;
    for (key, value) in url_pairs(query) {
        match key.as_str() {
            "state" => state = Some(value),
            "code" => code = Some(value),
            "error" => denied = Some(value),
            _ => {}
        }
    }
    if let Some(reason) = denied {
        bail!("Google refused the request: {reason}");
    }
    match (state, code) {
        (Some(state), Some(code)) if !state.is_empty() && !code.is_empty() => Ok((state, code)),
        _ => bail!("the callback is missing its `state` or `code`"),
    }
}

fn url_pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(key), decode(value))
        })
        .collect()
}

fn decode(value: &str) -> String {
    let plus_decoded = value.replace('+', " ");
    percent_encoding::percent_decode_str(&plus_decoded)
        .decode_utf8()
        .map_or(plus_decoded.clone(), std::borrow::Cow::into_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_authorize_url_carries_pkce_and_state() {
        let config = OAuthConfig::from_parts(
            "client-id",
            "client-secret",
            "http://localhost:8080/v1/auth/gmail/callback",
            "https://accounts.google.com/o/oauth2/auth",
            "https://oauth2.googleapis.com/token",
        )
        .unwrap();

        let (url, state, verifier) = config.authorize_url();
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/auth?"), "{url}");
        for fragment in [
            "code_challenge=",
            "code_challenge_method=S256",
            "state=",
            "access_type=offline",
            "prompt=consent",
            "response_type=code",
            "client_id=client-id",
        ] {
            assert!(url.contains(fragment), "{fragment} missing from {url}");
        }
        assert!(
            url.contains("gmail.readonly"),
            "the scope must be readonly: {url}"
        );
        assert!(!url.contains("client-secret"), "the secret must never be in a URL");
        assert!(state.len() >= 16, "state must be unguessable: {state:?}");
        assert!(verifier.len() >= 43, "PKCE verifier must be 43+ chars");

        // Two starts never collide.
        let (_, second_state, second_verifier) = config.authorize_url();
        assert_ne!(state, second_state);
        assert_ne!(verifier, second_verifier);
    }

    /// Criterion K2 - single use, and expiry.
    #[test]
    fn a_state_is_single_use_and_expires() {
        let pending = PendingAuths::new();
        pending.remember("abc".into(), "verifier".into());
        assert_eq!(pending.take("abc").as_deref(), Some("verifier"));
        assert_eq!(pending.take("abc"), None, "replay must be refused");
        assert_eq!(pending.take("never-issued"), None);
        assert!(pending.is_empty());
    }

    #[test]
    fn pending_consents_are_bounded() {
        let pending = PendingAuths::new();
        for index in 0..(MAX_PENDING * 3) {
            pending.remember(format!("state-{index}"), "v".into());
        }
        assert!(
            pending.len() <= MAX_PENDING,
            "an unauthenticated route must not grow memory without bound: {}",
            pending.len()
        );
    }

    #[test]
    fn callback_params_are_read_or_named() {
        assert_eq!(
            callback_params("state=s1&code=c1&scope=x").unwrap(),
            ("s1".to_owned(), "c1".to_owned())
        );
        // Percent- and plus-encoded values.
        assert_eq!(
            callback_params("state=a%2Fb&code=c%20d").unwrap(),
            ("a/b".to_owned(), "c d".to_owned())
        );
        // The user pressed Cancel.
        let error = callback_params("error=access_denied&state=s").unwrap_err().to_string();
        assert!(error.contains("access_denied"), "{error}");
        // EDGE (empty input).
        assert!(callback_params("").is_err());
        assert!(callback_params("state=&code=").is_err());
        assert!(callback_params("code=only").is_err());
    }

    #[test]
    fn a_client_file_without_redirects_still_loads() {
        let path = std::env::temp_dir().join(format!("nade-web-client-{}.json", Uuid::new_v4()));
        std::fs::write(
            &path,
            r#"{"web":{"client_id":"cid","client_secret":"sec",
                "auth_uri":"https://accounts.google.com/o/oauth2/auth",
                "token_uri":"https://oauth2.googleapis.com/token"}}"#,
        )
        .unwrap();

        let config = OAuthConfig::load(&path, None, None, None).unwrap();
        assert_eq!(
            config.redirect_uri,
            "http://localhost:8080/v1/auth/gmail/callback"
        );

        // The overrides the wiremock suite uses.
        let config = OAuthConfig::load(
            &path,
            Some("http://127.0.0.1:9/cb"),
            Some("http://127.0.0.1:9/auth"),
            Some("http://127.0.0.1:9/token"),
        )
        .unwrap();
        assert_eq!(config.token_url, "http://127.0.0.1:9/token");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_or_malformed_client_file_is_a_clear_error() {
        let missing = std::env::temp_dir().join("nade-definitely-not-here.json");
        let error = OAuthConfig::load(&missing, None, None, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Gmail client file"), "{error}");

        let path = std::env::temp_dir().join(format!("nade-bad-{}.json", Uuid::new_v4()));
        std::fs::write(&path, "{\"installed\":{}}").unwrap();
        assert!(OAuthConfig::load(&path, None, None, None).is_err());
        let _ = std::fs::remove_file(&path);
    }

    /// The real client file must stay loadable - if it drifts, the live run
    /// fails at the worst moment.
    #[test]
    fn the_checked_in_client_file_loads() {
        let path = std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../secrets/web_client.json"
        ));
        if !path.exists() {
            println!("skipped: {} is absent on this machine", path.display());
            return;
        }
        let config = OAuthConfig::load(
            &path,
            Some("http://localhost:8080/v1/auth/gmail/callback"),
            None,
            None,
        )
        .expect("secrets/web_client.json must load");
        assert_eq!(config.token_url, "https://oauth2.googleapis.com/token");
        let (url, _, _) = config.authorize_url();
        assert!(url.contains("code_challenge_method=S256"));
    }
}
