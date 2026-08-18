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

/// The advisory-lock key that makes "this server serves exactly one mailbox"
/// atomic rather than hopeful.
///
/// The value is arbitrary - the ASCII of `nadeacct`, so it is recognisable in
/// `pg_locks` - and what matters is only that **every** writer of `accounts`
/// takes the same one. Deliberately *not* a `unique index on accounts ((true))`:
/// six tests in `db.rs` insert several accounts on purpose, to prove that every
/// query is scoped per account, and a hard singleton index would forbid exactly
/// the tests that prove isolation.
pub const ACCOUNT_SINGLETON_LOCK: i64 = i64::from_be_bytes(*b"nadeacct");

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
            auth_override
                .or(file.web.auth_uri.as_deref())
                .unwrap_or("https://accounts.google.com/o/oauth2/auth"),
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
        (url.to_string(), state.into_secret(), verifier.into_secret())
    }
}

// ------------------------------------------------------ pending consents --

/// Everything `start` has to hand `callback`, keyed by the OAuth `state`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    /// Proves the callback belongs to the flow we started.
    pub verifier: String,
    /// Proves the callback arrived in the **same browser**: the value of the
    /// `HttpOnly` cookie `start` set. Without it, a `state` lifted out of the
    /// redirect (a shared screen, a proxy log, a referrer) would be enough to
    /// finish somebody else's consent.
    pub binding: String,
    /// The device whose capability opened this flow, so revoking it closes the
    /// flow too. `None` is the dev-token principal, which has no `devices` row.
    pub device_id: Option<Uuid>,
}

/// `state` → [`Pending`], single-use and expiring.
#[derive(Debug, Default)]
pub struct PendingAuths {
    entries: Mutex<HashMap<String, (Pending, Instant)>>,
}

impl PendingAuths {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn remember(&self, state: String, pending: Pending) {
        self.remember_at(state, pending, Instant::now());
    }

    /// Remember with a chosen birth instant.
    ///
    /// `pub(crate)` and test-seam-shaped on purpose: the ten-minute `state` TTL
    /// is otherwise a ten-minute test (CRITERIA K2a).
    fn remember_at(&self, state: String, pending: Pending, born: Instant) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        entries.retain(|_, (_, seen)| !expired(*seen, now));
        if entries.len() >= MAX_PENDING {
            // Drop the oldest rather than refusing a legitimate new consent.
            if let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, (_, seen))| *seen)
                .map(|(key, _)| key.clone())
            {
                entries.remove(&oldest);
            }
        }
        entries.insert(state, (pending, born));
    }

    /// Consume a `state`. `None` when it is unknown, already spent, or expired -
    /// which are deliberately indistinguishable to the caller.
    pub fn take(&self, state: &str) -> Option<Pending> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let (pending, born) = entries.remove(state)?;
        (!expired(born, Instant::now())).then_some(pending)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Plant an entry with a chosen birth instant, for the TTL tests.
    #[cfg(test)]
    pub fn remember_expiring_at(&self, state: String, pending: Pending, born: Instant) {
        self.remember_at(state, pending, born);
    }
}

/// EDGE (clock skew / clock running backwards): `Instant` is monotonic, so the
/// wall clock cannot lengthen a consent window, and `saturating_duration_since`
/// treats an entry stamped in the future as live rather than panicking.
fn expired(born: Instant, now: Instant) -> bool {
    now.saturating_duration_since(born) >= STATE_TTL
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
    /// A **different** mailbox already owns this server. v1 is single-account,
    /// and rebinding would silently repoint every stored message at a new owner.
    ///
    /// Raised from inside `save_consent`'s transaction, under the singleton
    /// advisory lock, which is what makes it authoritative rather than advisory:
    /// two callbacks racing for two different addresses cannot both win.
    #[error("this server is already connected to {existing}")]
    AlreadyBound { existing: String },
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
    ///
    /// **Returns a `Result`, and the caller must not ignore it.** This used to
    /// return `()`. When the `gmail_tokens` write failed, the 401 arm could not
    /// tell, re-sent the same dead token, burned its single auth retry and
    /// returned `needs_reauth` - so a database that blinked for one second told
    /// the user to reconnect Gmail. A failure here is transient and upstream,
    /// and saying so is the whole point of the signature.
    ///
    /// # Errors
    /// Returns [`TokenError`] if the invalidation could not be recorded.
    async fn invalidate(&self) -> Result<(), TokenError>;
}

/// A fixed token. Tests only.
#[derive(Debug)]
pub struct StaticTokens(pub String);

#[async_trait]
impl AccessTokens for StaticTokens {
    async fn access_token(&self) -> Result<String, TokenError> {
        Ok(self.0.clone())
    }
    async fn invalidate(&self) -> Result<(), TokenError> {
        Ok(())
    }
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
    pub async fn exchange_code(
        &self,
        code: &str,
        verifier: &str,
    ) -> Result<FreshTokens, TokenError> {
        let oauth = self.oauth.as_ref().ok_or(TokenError::NotConnected)?;
        let bridge = bridge(self.http.clone());
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
    /// **This is the authoritative singleton check.** The handler's
    /// [`existing_account_email`] pre-check is a courtesy that buys the friendly
    /// path; only the re-check below, inside this transaction and under
    /// [`ACCOUNT_SINGLETON_LOCK`], is a guarantee.
    ///
    /// # Errors
    /// [`TokenError::AlreadyBound`] when a different mailbox already owns this
    /// server, or an error if the write fails.
    pub async fn save_consent(
        &self,
        email: &str,
        tokens: &FreshTokens,
    ) -> Result<Uuid, TokenError> {
        let mut tx = self.pool.begin().await?;

        // First statement in the transaction, before anything is read: two
        // concurrent callbacks used to both observe "no account" and both
        // insert. `accounts.email` being unique did not stop them, because the
        // whole point of the race is that the two addresses differ.
        //
        // EDGE (crash mid-step): `pg_advisory_xact_lock` is released by COMMIT
        // *and* by ROLLBACK, and by the backend dying - so a consent that fails
        // half way cannot leave the lock held and wedge every later sign-in.
        sqlx::query("select pg_advisory_xact_lock($1)")
            .bind(ACCOUNT_SINGLETON_LOCK)
            .execute(&mut *tx)
            .await?;

        // EDGE (duplicate delivery): the *same* mailbox re-consenting is
        // idempotent and must not 409 - that is the `needs_reauth` recovery
        // path. Only a different address is refused, and case is ignored
        // because Gmail echoes whatever the user typed.
        let existing: Option<(Uuid, String)> =
            sqlx::query_as("select id, email from accounts order by created_at, id limit 1")
                .fetch_optional(&mut *tx)
                .await?;

        let account_id: Uuid = match existing {
            Some((id, bound)) => {
                if !bound.eq_ignore_ascii_case(email) {
                    return Err(TokenError::AlreadyBound { existing: bound });
                }
                // EDGE (case): `accounts.email` is unique but PostgreSQL text
                // is case-sensitive, so `on conflict (email)` would happily
                // insert a *second* row for the same mailbox spelled
                // differently. Update the row we already have instead.
                sqlx::query("update accounts set status = 'ok' where id = $1")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                id
            }
            // Under the lock there is provably no account, so this cannot
            // conflict; `on conflict` is kept only so a row inserted outside
            // the lock (a test fixture, a manual `psql`) is absorbed rather
            // than raising a unique violation.
            None => {
                sqlx::query_scalar(
                    "insert into accounts (email, status) values ($1, 'ok') \
                     on conflict (email) do update set status = 'ok' returning id",
                )
                .bind(email)
                .fetch_one(&mut *tx)
                .await?
            }
        };

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

        let bridge = bridge(self.http.clone());
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
    pub async fn mark_needs_reauth(
        &self,
        account_id: Uuid,
        reason: &str,
    ) -> Result<(), TokenError> {
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

    async fn invalidate(&self) -> Result<(), TokenError> {
        self.store.invalidate(self.account_id).await
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
///
/// The client is **cloned into** each call rather than borrowed. `reqwest::Client`
/// is an `Arc` inside, so a clone is free - and it makes the returned future
/// `'static`, which is what `oauth2`'s `AsyncHttpClient` blanket impl needs to
/// see a single concrete future type.
fn bridge(
    http: reqwest::Client,
) -> impl Fn(oauth2::HttpRequest) -> BridgeFuture + Send + Sync + 'static {
    move |request| {
        let http = http.clone();
        Box::pin(async move { send(http, request).await })
    }
}

type BridgeFuture = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<oauth2::HttpResponse, BridgeError>> + Send>,
>;

async fn send(
    http: reqwest::Client,
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
/// This is the **cheap pre-check**, outside any transaction, and it exists only
/// so the friendly path renders a friendly page. The guarantee lives in
/// [`TokenStore::save_consent`], under [`ACCOUNT_SINGLETON_LOCK`].
///
/// `order by created_at, id`: `created_at` alone is not a total order - two rows
/// inserted in the same transaction, or on a clock with coarse resolution, tie -
/// and "the account" must be the same row on every call.
///
/// # Errors
/// Returns an error if the query fails.
pub async fn existing_account_email(pool: &PgPool) -> Result<Option<String>> {
    let email: Option<String> =
        sqlx::query_scalar("select email from accounts order by created_at, id limit 1")
            .fetch_optional(pool)
            .await?;
    Ok(email)
}

/// Read a single query-string parameter.
///
/// EDGE (empty input / unicode): an absent key, an empty value and a value that
/// is not valid percent-encoded UTF-8 all come back as `None` or as the
/// undecoded text, never as a panic.
#[must_use]
pub fn query_param(query: &str, key: &str) -> Option<String> {
    url_pairs(query)
        .into_iter()
        .find(|(name, value)| name == key && !value.is_empty())
        .map(|(_, value)| value)
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
        assert!(
            url.starts_with("https://accounts.google.com/o/oauth2/auth?"),
            "{url}"
        );
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
        assert!(
            !url.contains("client-secret"),
            "the secret must never be in a URL"
        );
        assert!(state.len() >= 16, "state must be unguessable: {state:?}");
        assert!(verifier.len() >= 43, "PKCE verifier must be 43+ chars");

        // Two starts never collide.
        let (_, second_state, second_verifier) = config.authorize_url();
        assert_ne!(state, second_state);
        assert_ne!(verifier, second_verifier);
    }

    fn sample_pending() -> Pending {
        Pending {
            verifier: "verifier".into(),
            binding: "cookie".into(),
            device_id: None,
        }
    }

    /// Criterion K2 - single use, and expiry.
    #[test]
    fn a_state_is_single_use_and_expires() {
        let pending = PendingAuths::new();
        pending.remember("abc".into(), sample_pending());
        assert_eq!(pending.take("abc"), Some(sample_pending()));
        assert_eq!(pending.take("abc"), None, "replay must be refused");
        assert_eq!(pending.take("never-issued"), None);
        assert!(pending.is_empty());
    }

    /// Criterion K2a - the ten-minute `state` TTL itself, which used to be a
    /// `[~]` because waiting it out costs ten minutes per run. Planting the
    /// birth instant costs nothing.
    #[test]
    fn a_state_older_than_the_ttl_is_refused() {
        let pending = PendingAuths::new();
        let entry = sample_pending();

        let Some(long_ago) = Instant::now().checked_sub(STATE_TTL + Duration::from_secs(1)) else {
            println!("skipped: the monotonic clock has not been running for ten minutes");
            return;
        };
        pending.remember_expiring_at("stale".into(), entry.clone(), long_ago);
        assert_eq!(pending.take("stale"), None, "ten minutes is the budget");

        // The boundary is expired too: `>=`, not `>`.
        pending.remember_expiring_at(
            "edge".into(),
            entry.clone(),
            Instant::now().checked_sub(STATE_TTL).unwrap(),
        );
        assert_eq!(pending.take("edge"), None);

        // Nine minutes is still fine.
        pending.remember_expiring_at(
            "fresh".into(),
            entry,
            Instant::now().checked_sub(Duration::from_secs(540)).unwrap(),
        );
        assert!(pending.take("fresh").is_some());
    }

    #[test]
    fn pending_consents_are_bounded() {
        let pending = PendingAuths::new();
        for index in 0..(MAX_PENDING * 3) {
            pending.remember(format!("state-{index}"), sample_pending());
        }
        assert!(
            pending.len() <= MAX_PENDING,
            "an unauthenticated route must not grow memory without bound: {}",
            pending.len()
        );
    }

    #[test]
    fn query_params_are_read_or_absent() {
        assert_eq!(query_param("cap=abc", "cap").as_deref(), Some("abc"));
        assert_eq!(
            query_param("x=1&cap=a%2Fb&y=2", "cap").as_deref(),
            Some("a/b")
        );
        // EDGE (empty input): absent, blank, and valueless are all `None`.
        assert_eq!(query_param("", "cap"), None);
        assert_eq!(query_param("cap=", "cap"), None);
        assert_eq!(query_param("cap", "cap"), None);
        assert_eq!(query_param("capacity=x", "cap"), None);
        // EDGE (unicode): decodable and undecodable bytes both come back as a
        // string rather than a panic.
        assert_eq!(query_param("cap=%F0%9F%94%90", "cap").as_deref(), Some("🔐"));
        assert!(query_param("cap=%FF%FE", "cap").is_some());
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
        let error = callback_params("error=access_denied&state=s")
            .unwrap_err()
            .to_string();
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

    // ------------------------------------------------ the token lifecycle --

    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    struct Harness {
        _db: crate::test_support::TestDb,
        pool: PgPool,
        store: TokenStore,
        account: Uuid,
    }

    /// A real database, a real `TokenStore`, and a token endpoint we control.
    async fn harness(server: &MockServer) -> Harness {
        let db = crate::test_support::test_db().await;
        let config = OAuthConfig::from_parts(
            "client-id",
            "client-secret",
            "http://localhost:8080/v1/auth/gmail/callback",
            &format!("{}/auth", server.uri()),
            &format!("{}/token", server.uri()),
        )
        .unwrap();

        let store = TokenStore::new(
            db.pool.clone(),
            Cipher::from_key([5u8; 32]),
            Some(std::sync::Arc::new(config)),
            crate::gmail::http_client().unwrap(),
        );

        let account = store
            .save_consent(
                "jatinsethi98@gmail.com",
                &FreshTokens {
                    access_token: "access-original".to_owned(),
                    refresh_token: Some("refresh-original".to_owned()),
                    expires_at: Some(Utc::now() + chrono::TimeDelta::seconds(3600)),
                    scopes: vec![SCOPE.to_owned()],
                },
            )
            .await
            .unwrap();

        Harness {
            pool: db.pool.clone(),
            _db: db,
            store,
            account,
        }
    }

    fn token_response(refresh: Option<&str>) -> String {
        let rotation = refresh.map_or_else(String::new, |token| {
            format!(r#","refresh_token":"{token}""#)
        });
        format!(
            r#"{{"access_token":"access-refreshed","token_type":"Bearer","expires_in":3599,
                 "scope":"{SCOPE}"{rotation}}}"#
        )
    }

    async fn expire_the_access_token(pool: &PgPool, account: Uuid) {
        sqlx::query("update gmail_tokens set access_expiry = now() - interval '5 minutes' where account_id = $1")
            .bind(account)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn stored_refresh(harness: &Harness) -> String {
        let sealed: String =
            sqlx::query_scalar("select refresh_token from gmail_tokens where account_id = $1")
                .bind(harness.account)
                .fetch_one(&harness.pool)
                .await
                .unwrap();
        harness.store.cipher.decrypt(&sealed).unwrap()
    }

    /// Criterion K4 - the failure that killed this project's prior art. Google
    /// rotates the refresh token; keep the old one and you get `invalid_grant`
    /// forever.
    #[tokio::test]
    async fn a_rotated_refresh_token_is_persisted() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                token_response(Some("refresh-ROTATED")).into_bytes(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let harness = harness(&server).await;
        assert_eq!(stored_refresh(&harness).await, "refresh-original");

        expire_the_access_token(&harness.pool, harness.account).await;
        let token = harness.store.access_token(harness.account).await.unwrap();

        assert_eq!(token, "access-refreshed");
        assert_eq!(
            stored_refresh(&harness).await,
            "refresh-ROTATED",
            "the rotated refresh token must be written back on the spot"
        );
    }

    /// Criterion K5 - the mirror image: no rotation must not mean "null it".
    #[tokio::test]
    async fn a_refresh_without_rotation_keeps_the_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(token_response(None).into_bytes(), "application/json"),
            )
            .mount(&server)
            .await;

        let harness = harness(&server).await;
        expire_the_access_token(&harness.pool, harness.account).await;
        assert_eq!(
            harness.store.access_token(harness.account).await.unwrap(),
            "access-refreshed"
        );
        assert_eq!(
            stored_refresh(&harness).await,
            "refresh-original",
            "a refresh that did not rotate must leave the token alone"
        );
    }

    /// Criterion K8 + K10 - a live token is reused; one inside the skew margin
    /// is not.
    #[tokio::test]
    async fn a_live_token_is_not_refreshed_but_a_nearly_dead_one_is() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(token_response(None).into_bytes(), "application/json"),
            )
            .mount(&server)
            .await;

        let harness = harness(&server).await;

        // An hour of life left: reused, and no HTTP call at all.
        assert_eq!(
            harness.store.access_token(harness.account).await.unwrap(),
            "access-original"
        );
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "a live token must not cost a refresh"
        );

        // EDGE (clock skew): 30 seconds of life is inside the 60-second margin,
        // so it is refreshed *before* it can 401 in the middle of a batch.
        sqlx::query(
            "update gmail_tokens set access_expiry = now() + interval '30 seconds' \
              where account_id = $1",
        )
        .bind(harness.account)
        .execute(&harness.pool)
        .await
        .unwrap();

        assert_eq!(
            harness.store.access_token(harness.account).await.unwrap(),
            "access-refreshed",
            "a token expiring within the skew margin must be refreshed early"
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    /// Criteria K6 + K7 - the whole `invalid_grant` lifecycle, and its
    /// idempotence.
    #[tokio::test]
    async fn invalid_grant_marks_needs_reauth_exactly_once() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_raw(
                br#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#
                    .to_vec(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let harness = harness(&server).await;
        expire_the_access_token(&harness.pool, harness.account).await;

        let error = harness
            .store
            .access_token(harness.account)
            .await
            .unwrap_err();
        assert!(matches!(error, TokenError::NeedsReauth), "{error}");

        let status: String = sqlx::query_scalar("select status from accounts where id = $1")
            .bind(harness.account)
            .fetch_one(&harness.pool)
            .await
            .unwrap();
        assert_eq!(status, "needs_reauth", "sync pauses off this flag");

        let feed: i64 = sqlx::query_scalar(
            "select count(*) from feed_items \
              where account_id = $1 and kind = 'info' and data->>'reason' = 'needs_reauth'",
        )
        .bind(harness.account)
        .fetch_one(&harness.pool)
        .await
        .unwrap();
        assert_eq!(feed, 1, "the user is told once");

        let audit: i64 = sqlx::query_scalar(
            "select count(*) from audit_log where action = 'gmail_needs_reauth'",
        )
        .fetch_one(&harness.pool)
        .await
        .unwrap();
        assert_eq!(audit, 1);

        // EDGE (duplicate delivery): a second failure must not stack a second
        // card on the feed.
        let error = harness
            .store
            .access_token(harness.account)
            .await
            .unwrap_err();
        assert!(matches!(error, TokenError::NeedsReauth), "{error}");

        let feed: i64 = sqlx::query_scalar(
            "select count(*) from feed_items \
              where account_id = $1 and kind = 'info' and data->>'reason' = 'needs_reauth'",
        )
        .bind(harness.account)
        .fetch_one(&harness.pool)
        .await
        .unwrap();
        assert_eq!(feed, 1, "repeated failures must not spam the feed");

        // Recovery is another trip through /auth/gmail/start, which re-consents.
        harness
            .store
            .save_consent(
                "jatinsethi98@gmail.com",
                &FreshTokens {
                    access_token: "access-after-reconsent".to_owned(),
                    refresh_token: Some("refresh-after-reconsent".to_owned()),
                    expires_at: Some(Utc::now() + chrono::TimeDelta::seconds(3600)),
                    scopes: vec![SCOPE.to_owned()],
                },
            )
            .await
            .unwrap();

        let status: String = sqlx::query_scalar("select status from accounts where id = $1")
            .bind(harness.account)
            .fetch_one(&harness.pool)
            .await
            .unwrap();
        assert_eq!(status, "ok");
        let unresolved: i64 = sqlx::query_scalar(
            "select count(*) from feed_items where status = 'new' and data->>'reason' = 'needs_reauth'",
        )
        .fetch_one(&harness.pool)
        .await
        .unwrap();
        assert_eq!(unresolved, 0, "re-consent clears the card");
        assert_eq!(
            harness.store.access_token(harness.account).await.unwrap(),
            "access-after-reconsent"
        );
    }

    /// Criterion K9 - a `pg_dump` must not hand over the account.
    #[tokio::test]
    async fn tokens_are_ciphertext_at_rest() {
        let server = MockServer::start().await;
        let harness = harness(&server).await;

        let (access, refresh): (String, String) = sqlx::query_as(
            "select access_token, refresh_token from gmail_tokens where account_id = $1",
        )
        .bind(harness.account)
        .fetch_one(&harness.pool)
        .await
        .unwrap();

        for column in [&access, &refresh] {
            assert!(!column.contains("access-original"), "{column}");
            assert!(!column.contains("refresh-original"), "{column}");
        }
        assert_eq!(
            harness.store.cipher.decrypt(&access).unwrap(),
            "access-original"
        );
        assert_eq!(stored_refresh(&harness).await, "refresh-original");
    }

    /// A concurrent burst must spend the rotating refresh token exactly once.
    #[tokio::test]
    async fn concurrent_refreshes_do_not_race() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                token_response(Some("refresh-rotated-once")).into_bytes(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let harness = harness(&server).await;
        expire_the_access_token(&harness.pool, harness.account).await;

        let store = std::sync::Arc::new(harness.store);
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let store = std::sync::Arc::clone(&store);
                let account = harness.account;
                tokio::spawn(async move { store.access_token(account).await })
            })
            .collect();
        for task in tasks {
            assert_eq!(task.await.unwrap().unwrap(), "access-refreshed");
        }

        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "eight callers, one refresh - two would burn the rotating token"
        );
    }

    // ------------------------------------------- the singleton account (F2) --

    /// A store over a private database with no account bound yet. `oauth: None`
    /// because `save_consent` never touches the OAuth client.
    fn bare_store(pool: PgPool) -> TokenStore {
        TokenStore::new(
            pool,
            Cipher::from_key([7u8; 32]),
            None,
            crate::gmail::http_client().unwrap(),
        )
    }

    fn consent_for(email: &str) -> FreshTokens {
        FreshTokens {
            access_token: format!("access-for-{email}"),
            refresh_token: Some(format!("refresh-for-{email}")),
            expires_at: Some(Utc::now() + chrono::TimeDelta::seconds(3600)),
            scopes: vec![SCOPE.to_owned()],
        }
    }

    /// Race `save_consent` for each address, released together.
    async fn race_consents(
        store: &std::sync::Arc<TokenStore>,
        emails: [&'static str; 2],
    ) -> Vec<Result<(&'static str, Uuid), TokenError>> {
        let gate = std::sync::Arc::new(tokio::sync::Barrier::new(emails.len()));
        let tasks: Vec<_> = emails
            .into_iter()
            .map(|email| {
                let store = std::sync::Arc::clone(store);
                let gate = std::sync::Arc::clone(&gate);
                tokio::spawn(async move {
                    gate.wait().await;
                    store
                        .save_consent(email, &consent_for(email))
                        .await
                        .map(|id| (email, id))
                })
            })
            .collect();

        let mut outcomes = Vec::new();
        for task in tasks {
            outcomes.push(task.await.expect("no consent task may panic"));
        }
        outcomes
    }

    /// F2 - the defect. `existing_account_email` and `save_consent` used to run
    /// in **separate** transactions, so two callbacks for two different
    /// addresses both observed "no account" and both inserted; `accounts.email`
    /// being unique did not help, because the addresses differ. One survives.
    #[tokio::test]
    async fn two_racing_consents_bind_exactly_one_account() {
        let db = crate::test_support::test_db().await;
        let store = std::sync::Arc::new(bare_store(db.pool.clone()));

        let outcomes = race_consents(&store, ["jatinsethi98@gmail.com", "impostor@gmail.com"]).await;

        let mut winner = None;
        let mut refusals = Vec::new();
        for outcome in outcomes {
            match outcome {
                Ok((email, _)) => {
                    assert!(winner.is_none(), "two winners is the bug itself");
                    winner = Some(email);
                }
                Err(TokenError::AlreadyBound { existing }) => refusals.push(existing),
                Err(other) => panic!("the loser must be AlreadyBound, not {other}"),
            }
        }

        let winner = winner.expect("one of the two consents has to succeed");
        assert_eq!(refusals, vec![winner.to_owned()], "the 409 names the winner");

        let emails: Vec<String> =
            sqlx::query_scalar("select email from accounts order by created_at, id")
                .fetch_all(&db.pool)
                .await
                .unwrap();
        assert_eq!(
            emails,
            vec![winner.to_owned()],
            "exactly one accounts row may survive the race"
        );

        // And the loser wrote nothing at all - no tokens, no audit row.
        let tokens: i64 = sqlx::query_scalar("select count(*) from gmail_tokens")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(tokens, 1);
        let audited: i64 =
            sqlx::query_scalar("select count(*) from audit_log where action = 'gmail_consent'")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(audited, 1, "the losing transaction rolled back whole");
    }

    /// EDGE (duplicate delivery): two callbacks for the **same** mailbox are
    /// re-consent, not a conflict - that is the `needs_reauth` recovery path,
    /// and it must stay idempotent under a race. The case difference is the
    /// nastier half: `accounts.email` is unique, but PostgreSQL text is
    /// case-sensitive, so an `on conflict (email)` upsert would have made two
    /// rows for one mailbox.
    #[tokio::test]
    async fn racing_re_consent_for_the_same_email_is_idempotent() {
        let db = crate::test_support::test_db().await;
        let store = std::sync::Arc::new(bare_store(db.pool.clone()));

        let outcomes = race_consents(
            &store,
            ["jatinsethi98@gmail.com", "JatinSethi98@Gmail.com"],
        )
        .await;

        let ids: Vec<Uuid> = outcomes
            .into_iter()
            .map(|outcome| outcome.expect("re-consent must never 409").1)
            .collect();
        assert_eq!(ids.len(), 2, "both consents succeed");
        assert_eq!(ids[0], ids[1], "and both land on the same account row");

        let rows: i64 = sqlx::query_scalar("select count(*) from accounts")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(rows, 1, "one mailbox is one row, whatever its spelling");
    }

    /// F2 - `order by created_at` alone is not a total order. Two rows stamped
    /// at the same instant made "the account" an arbitrary choice that could
    /// differ between two calls in the same request.
    #[tokio::test]
    async fn the_singleton_account_query_is_deterministic() {
        let db = crate::test_support::test_db().await;
        // The same `created_at` on purpose - `now()` is transaction time, so a
        // single statement inserting two rows already produces this.
        sqlx::query(
            "insert into accounts (email, created_at) values \
                 ('b@example.com', timestamptz '2026-01-01 00:00:00Z'), \
                 ('a@example.com', timestamptz '2026-01-01 00:00:00Z')",
        )
        .execute(&db.pool)
        .await
        .unwrap();

        let expected: Uuid =
            sqlx::query_scalar("select id from accounts order by created_at, id limit 1")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        let expected_email: String = sqlx::query_scalar("select email from accounts where id = $1")
            .bind(expected)
            .fetch_one(&db.pool)
            .await
            .unwrap();

        for _ in 0..5 {
            assert_eq!(
                existing_account_email(&db.pool).await.unwrap().as_deref(),
                Some(expected_email.as_str())
            );
            let state = crate::state::AppState::new(
                db.pool.clone(),
                crate::test_support::test_config(crate::config::Env::Prod),
            );
            assert_eq!(state.account().await.unwrap().map(|a| a.id), Some(expected));
        }
    }

    /// The lock key is a constant other code will one day have to match. Pin it,
    /// so "take the same lock" is checkable rather than aspirational - and pin
    /// that it stays inside a **signed** bigint, which is what
    /// `pg_advisory_xact_lock` takes.
    #[test]
    fn the_singleton_lock_key_is_stable() {
        assert_eq!(ACCOUNT_SINGLETON_LOCK, 0x6e61_6465_6163_6374);
        assert_eq!(
            ACCOUNT_SINGLETON_LOCK.to_be_bytes(),
            *b"nadeacct",
            "the key is the ASCII, so it is recognisable in pg_locks"
        );
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
