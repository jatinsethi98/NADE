//! Everything a handler needs, cloned per request.

use std::sync::Arc;

use sqlx::PgPool;

use crate::{
    api::{
        auth::{pairing::PairingStore, RateLimiter},
        gmail_auth::capability::LinkCapabilities,
    },
    config::Config,
    gmail::GmailRuntime,
};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: Arc<Config>,
    pub pairing: Arc<PairingStore>,
    /// Guards `POST /v1/auth/pair` against brute force. Process-wide rather
    /// than per-IP: the 6-digit space is small enough that a global cap is the
    /// honest defence, and pairing is rare enough that users never contend.
    pub pair_limiter: Arc<RateLimiter>,
    /// OAuth, the token store, the per-account quota buckets and the REST
    /// endpoints.
    pub gmail: Arc<GmailRuntime>,
    /// Single-use permission slips for `GET /v1/auth/gmail/start`, minted only
    /// behind the bearer guard. Lives here rather than on [`GmailRuntime`]
    /// because it is an *API* concern: it is what makes a browser-facing route
    /// authorised without a header.
    pub gmail_link: Arc<LinkCapabilities>,
    /// Verifies Gmail's Pub/Sub push. Holds the cached JWK Set, so it must be
    /// shared rather than rebuilt per request.
    pub push: Arc<crate::api::webhooks::oidc::Verifier>,
}

impl AppState {
    /// # Panics
    /// Panics only if the token encryption key is present but unusable, which is
    /// a misconfiguration the process must not start with.
    #[must_use]
    pub fn new(pool: PgPool, config: Config) -> Self {
        Self::try_new(pool, config).expect("building the application state")
    }

    /// # Errors
    /// Returns an error if the Gmail token encryption key cannot be resolved.
    /// A *missing* OAuth client file is not an error - the server still boots.
    pub fn try_new(pool: PgPool, config: Config) -> anyhow::Result<Self> {
        let pairing = Arc::new(PairingStore::new(&config.pairing));
        let pair_limiter = Arc::new(RateLimiter::new(
            config.pairing.rate_limit,
            config.pairing.rate_window,
        ));
        let gmail = Arc::new(GmailRuntime::build(pool.clone(), &config)?);
        // The same shared client the Gmail calls use: the crate takes
        // `rustls-no-provider`, so a `reqwest::Client::new()` here would panic
        // at construction (`gmail::tests::no_bare_reqwest_clients`).
        let push = Arc::new(crate::api::webhooks::oidc::Verifier::new(
            gmail.http.clone(),
            config.push.clone(),
        ));
        Ok(Self {
            pool,
            config: Arc::new(config),
            pairing,
            pair_limiter,
            gmail,
            gmail_link: Arc::new(LinkCapabilities::new()),
            push,
        })
    }

    /// The **sole** account on this server - `Some` iff exactly one exists.
    ///
    /// This is the fallback half of account resolution (backend/DECISIONS.md
    /// D45): the resolved account is the authenticated device's *bound* one,
    /// and only an unbound principal falls back here. With two or more
    /// accounts there is no honest answer to "the account", so the fallback
    /// refuses rather than picking one - the old `limit 1` would have handed
    /// an arbitrary user's mailbox to every unbound device.
    ///
    /// The name is the contract: no caller may mean "the current user" by it.
    /// Legitimate callers are the resolver's fallback and the job handlers'
    /// payload fallback (a job row with no `account_id`).
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub async fn sole_account(&self) -> sqlx::Result<Option<Account>> {
        // `limit 2`, not a count: one row answers "which", two rows answer
        // "too many", and either way it is one cheap index read.
        let mut rows = sqlx::query_as::<_, Account>(
            "select id, email, status from accounts order by created_at, id limit 2",
        )
        .fetch_all(&self.pool)
        .await?;
        if rows.len() == 1 {
            Ok(rows.pop())
        } else {
            Ok(None)
        }
    }
}

/// One `accounts` row - the shape every resolved-account consumer reads.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Account {
    pub id: uuid::Uuid,
    pub email: String,
    /// `ok` | `needs_reauth`.
    pub status: String,
}

/// Deliberately hand-written: `Config` holds `NADE_TOKEN` and the dev database
/// password, and a derived `Debug` would put both in any log line that ever
/// formats the state.
impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("env", &self.config.env)
            .field("pairing_state_file", &self.pairing.path())
            .field("gmail", &self.gmail)
            .finish_non_exhaustive()
    }
}
