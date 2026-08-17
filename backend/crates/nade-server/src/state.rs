//! Everything a handler needs, cloned per request.

use std::sync::Arc;

use sqlx::PgPool;

use crate::{
    api::auth::{pairing::PairingStore, RateLimiter},
    config::Config,
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
}

impl AppState {
    #[must_use]
    pub fn new(pool: PgPool, config: Config) -> Self {
        let pairing = Arc::new(PairingStore::new(&config.pairing));
        let pair_limiter = Arc::new(RateLimiter::new(
            config.pairing.rate_limit,
            config.pairing.rate_window,
        ));
        Self {
            pool,
            config: Arc::new(config),
            pairing,
            pair_limiter,
        }
    }
}

/// Deliberately hand-written: `Config` holds `NADE_TOKEN` and the dev database
/// password, and a derived `Debug` would put both in any log line that ever
/// formats the state.
impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("env", &self.config.env)
            .field("pairing_state_file", &self.pairing.path())
            .finish_non_exhaustive()
    }
}
