//! Everything a handler needs, cloned per request.

use std::sync::Arc;

use sqlx::PgPool;

use crate::{
    api::auth::{pairing::PairingStore, RateLimiter},
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
    /// honest defence, and NADE is a single-user server.
    pub pair_limiter: Arc<RateLimiter>,
    /// OAuth, the token store, the quota bucket and the REST endpoints.
    pub gmail: Arc<GmailRuntime>,
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
        Ok(Self {
            pool,
            config: Arc::new(config),
            pairing,
            pair_limiter,
            gmail,
        })
    }

    /// The single account this server serves, if one has been connected.
    ///
    /// v1 is single-account by design (`API.md` §0), so "the account" is a
    /// query rather than a parameter on every route.
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub async fn account(&self) -> sqlx::Result<Option<Account>> {
        sqlx::query_as::<_, Account>(
            "select id, email, status from accounts order by created_at limit 1",
        )
        .fetch_optional(&self.pool)
        .await
    }
}

/// The single `accounts` row.
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
