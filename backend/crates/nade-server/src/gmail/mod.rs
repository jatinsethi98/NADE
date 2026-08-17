//! Gmail: OAuth, the REST client, the quota bucket, and the batch endpoint.
//!
//! Raw `reqwest` against the REST API - no Google SDK. PLAN.md §Gmail sync is
//! the specification; the two things it is emphatic about, and that this module
//! exists to get right, are:
//!
//! * **persist every refresh** (Google rotates refresh tokens), and
//! * **debit the true quota cost** (250 units/user/second, `messages.get` = 5,
//!   so the ceiling is 50 gets/second and not 250 requests).

pub mod batch;
pub mod client;
pub mod crypto;
pub mod oauth;
pub mod quota;
pub mod types;

use std::sync::Arc;

use anyhow::Result;

use crate::config::Config;

/// Everything Gmail-shaped that a handler or an endpoint needs, built once at
/// start-up and cloned into [`crate::state::AppState`].
pub struct GmailRuntime {
    /// `None` when `secrets/web_client.json` is absent - the server still boots
    /// and serves every non-Gmail route, and `/v1/auth/gmail/start` says so
    /// plainly instead of panicking.
    pub oauth: Option<Arc<oauth::OAuthConfig>>,
    pub pending: Arc<oauth::PendingAuths>,
    pub tokens: Arc<oauth::TokenStore>,
    pub quota: Arc<quota::Bucket>,
    pub endpoints: client::Endpoints,
    pub http: reqwest::Client,
}

impl std::fmt::Debug for GmailRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GmailRuntime")
            .field("configured", &self.oauth.is_some())
            .field("endpoints", &self.endpoints)
            .finish_non_exhaustive()
    }
}

impl GmailRuntime {
    /// Build from configuration. Never fails on a missing client file: a
    /// developer without `secrets/web_client.json` should still get a working
    /// server, and `just ci` should still pass on a fresh clone.
    ///
    /// # Errors
    /// Returns an error only if the token encryption key cannot be resolved,
    /// which is a real misconfiguration rather than an absence.
    pub fn build(pool: sqlx::PgPool, config: &Config) -> Result<Self> {
        let http = http_client()?;

        let oauth = match oauth::OAuthConfig::load(
            &config.gmail.client_file,
            Some(&config.gmail.redirect_uri),
            config.gmail.auth_url.as_deref(),
            config.gmail.token_url.as_deref(),
        ) {
            Ok(loaded) => Some(Arc::new(loaded)),
            Err(error) => {
                tracing::warn!(
                    path = %config.gmail.client_file.display(),
                    error = %format!("{error:#}"),
                    "Gmail OAuth is not configured; /v1/auth/gmail/start will say so"
                );
                None
            }
        };

        let cipher = crypto::Cipher::resolve(
            config.gmail.token_key.as_deref(),
            &config.gmail.token_key_file,
        )?;

        Ok(Self {
            tokens: Arc::new(oauth::TokenStore::new(
                pool,
                cipher,
                oauth.clone(),
                http.clone(),
            )),
            oauth,
            pending: Arc::new(oauth::PendingAuths::new()),
            quota: Arc::new(quota::Bucket::new()),
            endpoints: config.gmail.endpoints.clone(),
            http,
        })
    }

    /// A client holding one fixed access token, for the single call the OAuth
    /// callback makes before an account row exists: "who just consented?".
    #[must_use]
    pub fn probe_client(&self, access_token: &str) -> client::GmailClient {
        client::GmailClient::new(
            self.http.clone(),
            self.endpoints.clone(),
            Arc::clone(&self.quota),
            Arc::new(oauth::StaticTokens(access_token.to_owned())),
        )
    }

    /// A client bound to one account.
    #[must_use]
    pub fn client_for(&self, account_id: uuid::Uuid) -> client::GmailClient {
        client::GmailClient::new(
            self.http.clone(),
            self.endpoints.clone(),
            Arc::clone(&self.quota),
            Arc::new(oauth::AccountTokens {
                store: Arc::clone(&self.tokens),
                account_id,
            }),
        )
    }
}

/// One shared `reqwest` client, with `ring` installed as rustls' provider.
///
/// `reqwest`'s default is `aws-lc-rs`, which needs cmake; `sqlx` already links
/// `ring`, and two crypto providers in one process is a trap. So we take
/// `rustls-no-provider` and install `ring` ourselves, exactly once.
///
/// # Errors
/// Returns an error if the TLS stack cannot be built.
pub fn http_client() -> Result<reqwest::Client> {
    static PROVIDER: std::sync::Once = std::sync::Once::new();
    PROVIDER.call_once(|| {
        // `install_default` errors only when something already installed one,
        // which is fine - we just need *a* provider to exist.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });

    reqwest::Client::builder()
        .user_agent(concat!("nade/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(60))
        .connect_timeout(std::time::Duration::from_secs(10))
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .build()
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole reason for the `rustls-no-provider` dance: this must not panic
    /// or fail, and calling it twice must be safe.
    #[test]
    fn the_http_client_builds_with_one_crypto_provider() {
        let first = http_client().expect("reqwest must build with the ring provider");
        let second = http_client().expect("building a second client must be safe");
        drop((first, second));
    }

    /// `reqwest::Client::new()` / `::builder()` **panic** in this crate: we take
    /// `rustls-no-provider`, so a client must come from [`http_client`], which
    /// installs `ring` first. Enforced by a grep, exactly like the ban on
    /// compile-time sqlx macros (backend/DECISIONS.md D1) - a comment would not
    /// have stopped it, and the failure mode is a panic in production.
    #[test]
    fn no_bare_reqwest_clients() {
        let root = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
        let banned = [
            concat!("reqwest::Client::", "new()"),
            concat!("reqwest::Client::", "builder()"),
        ];
        let mut offenders = Vec::new();
        let mut stack = vec![root];

        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|extension| extension != "rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                for (number, line) in text.lines().enumerate() {
                    let trimmed = line.trim_start();
                    // The one legitimate call site lives in this module.
                    if trimmed.starts_with("//") || path.ends_with("gmail/mod.rs") {
                        continue;
                    }
                    if banned.iter().any(|needle| line.contains(needle)) {
                        offenders.push(format!("{}:{}: {}", path.display(), number + 1, trimmed));
                    }
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "build reqwest clients with gmail::http_client(), not directly:\n{}",
            offenders.join("\n")
        );
    }
}
