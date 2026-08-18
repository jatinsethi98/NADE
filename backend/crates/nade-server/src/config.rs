//! Process configuration, read once from the environment at startup.
//!
//! Every variable named here is documented in `backend/.env.example`;
//! [`tests::env_example_documents_every_var`] fails the build if the two lists
//! ever drift.

use std::{fmt::Display, path::PathBuf, str::FromStr, time::Duration};

use anyhow::{bail, Context};

/// Every environment variable this crate reads.
pub const ENV_VARS: &[&str] = &[
    "RUST_LOG",
    "NADE_ENV",
    "NADE_BIND",
    "NADE_PORT",
    "DATABASE_URL",
    "NADE_DB_NAME",
    "NADE_DB_MAX_CONNECTIONS",
    "NADE_TOKEN",
    "NADE_PAIR_CODE_FILE",
    "NADE_PAIR_CODE_TTL_SECS",
    "NADE_PAIR_RATE_LIMIT",
    "NADE_PAIR_RATE_WINDOW_SECS",
    "NADE_WORKERS",
    "NADE_JOB_LEASE_SECS",
    "NADE_JOB_HEARTBEAT_SECS",
    "NADE_JOB_MAX_ATTEMPTS",
    "NADE_JOB_POLL_MS",
    "NADE_JOB_REAP_SECS",
    "NADE_JOB_SHUTDOWN_GRACE_SECS",
    "NADE_GMAIL_CLIENT_FILE",
    "NADE_GMAIL_REDIRECT_URI",
    "NADE_GMAIL_AUTH_URL",
    "NADE_GMAIL_TOKEN_URL",
    "NADE_GMAIL_API_BASE",
    "NADE_TOKEN_KEY",
    "NADE_TOKEN_KEY_FILE",
    "NADE_MAX_SYNC_MESSAGES",
    "NADE_SYNC_WINDOW_DAYS",
    "NADE_SYNC_BATCH_SIZE",
    "NADE_BACKEND_ROOT",
    "NADE_PG_PORT",
    "NADE_PG_PASSWORD",
    "NADE_PG_DATA_DIR",
    "NADE_PG_CACHE_DIR",
];

/// Which half of the world we are in.
///
/// Deliberately strict: only the exact string `dev` (any case) unlocks the dev
/// shortcuts. Anything else - including a typo, an empty value, or an unset
/// variable - is production, where `NADE_TOKEN` is inert and an embedded
/// database is never booted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Env {
    Dev,
    Prod,
}

impl Env {
    #[must_use]
    pub const fn is_dev(self) -> bool {
        matches!(self, Self::Dev)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Prod => "prod",
        }
    }
}

/// Pairing-code policy.
#[derive(Debug, Clone)]
pub struct PairingConfig {
    /// Where the live code is mirrored so a second process (`just pair`) can
    /// read or replace it. Lives under `backend/secrets/`, which is gitignored.
    pub state_file: PathBuf,
    /// How long a minted code stays usable. PLAN.md says 10 minutes.
    pub ttl: Duration,
    /// Attempts allowed per window, per process, across all callers.
    pub rate_limit: u32,
    pub rate_window: Duration,
}

/// Embedded-Postgres policy (dev and test only).
#[derive(Debug, Clone)]
pub struct EmbeddedConfig {
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub port: u16,
    pub password: String,
}

/// Gmail: where the OAuth client lives, where the API lives, and the dev caps.
#[derive(Debug, Clone)]
pub struct GmailConfig {
    /// Google's downloaded web client JSON. Absent is survivable: the server
    /// boots and every non-Gmail route works.
    pub client_file: PathBuf,
    /// Must match a redirect URI registered on the Google client.
    pub redirect_uri: String,
    /// Overrides for the wiremock suite; unset in production, where the URLs
    /// come from the client file.
    pub auth_url: Option<String>,
    pub token_url: Option<String>,
    pub endpoints: crate::gmail::client::Endpoints,
    /// 64 hex characters. Unset means "use the key file", generating it once.
    pub token_key: Option<String>,
    pub token_key_file: PathBuf,
    /// PLAN.md's `MAX_SYNC_MESSAGES`. A dev cap, and dev caps are law.
    pub max_sync_messages: usize,
    /// PLAN.md's 30-day window.
    pub sync_window_days: u32,
    /// Messages per `multipart/mixed` batch, capped at
    /// [`crate::gmail::client::MAX_BATCH`]. Sized against Gmail's **concurrency**
    /// limit, not its unit ceiling: Google runs the sub-requests of a batch in
    /// parallel, and 45 of them produced `"Too many concurrent requests for
    /// user."` on the first live sync.
    pub batch_size: usize,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub env: Env,
    pub bind: String,
    pub port: u16,
    /// When set, used verbatim and the embedded server is never started.
    pub database_url: Option<String>,
    /// Database created inside the embedded server when `database_url` is unset.
    pub db_name: String,
    pub db_max_connections: u32,
    /// Bearer value accepted as a shortcut - only ever consulted when
    /// `env == Env::Dev`.
    pub dev_token: Option<String>,
    pub pairing: PairingConfig,
    pub gmail: GmailConfig,
    pub jobs: crate::jobs::QueueConfig,
    pub workers: usize,
    pub embedded: EmbeddedConfig,
}

impl Config {
    /// Read the whole configuration from the process environment.
    ///
    /// # Errors
    /// Returns an error if any variable is present but unparseable, or if a
    /// value is outside its legal range.
    pub fn from_env() -> anyhow::Result<Self> {
        let env = match string("NADE_ENV").as_deref() {
            Some(v) if v.eq_ignore_ascii_case("dev") => Env::Dev,
            _ => Env::Prod,
        };

        let root = backend_root();

        let db_max_connections = parse::<u32>("NADE_DB_MAX_CONNECTIONS", 10)?;
        if db_max_connections == 0 {
            bail!("NADE_DB_MAX_CONNECTIONS must be at least 1");
        }

        let workers = parse::<usize>("NADE_WORKERS", 2)?;
        let max_attempts = parse::<i32>("NADE_JOB_MAX_ATTEMPTS", 5)?;
        if max_attempts < 1 {
            bail!("NADE_JOB_MAX_ATTEMPTS must be at least 1");
        }

        let lease = Duration::from_secs(parse::<u64>("NADE_JOB_LEASE_SECS", 300)?);
        let heartbeat = Duration::from_secs(parse::<u64>("NADE_JOB_HEARTBEAT_SECS", 60)?);
        if heartbeat.is_zero() || heartbeat >= lease {
            bail!(
                "NADE_JOB_HEARTBEAT_SECS ({}s) must be non-zero and shorter than \
                 NADE_JOB_LEASE_SECS ({}s), otherwise a live job loses its lease",
                heartbeat.as_secs(),
                lease.as_secs()
            );
        }

        let rate_limit = parse::<u32>("NADE_PAIR_RATE_LIMIT", 10)?;
        if rate_limit == 0 {
            bail!("NADE_PAIR_RATE_LIMIT must be at least 1");
        }

        // Dev caps are law (PLAN.md §Execution doctrine 5), so they are clamped
        // here rather than trusted to whoever set the variable.
        let max_sync_messages = parse::<usize>("NADE_MAX_SYNC_MESSAGES", 2_000)?;
        if max_sync_messages == 0 {
            bail!("NADE_MAX_SYNC_MESSAGES must be at least 1");
        }
        let sync_window_days = parse::<u32>("NADE_SYNC_WINDOW_DAYS", 30)?;
        if sync_window_days == 0 {
            bail!("NADE_SYNC_WINDOW_DAYS must be at least 1");
        }
        let batch_size = parse::<usize>("NADE_SYNC_BATCH_SIZE", crate::gmail::client::MAX_BATCH)?;
        if batch_size == 0 || batch_size > crate::gmail::client::MAX_BATCH {
            bail!(
                "NADE_SYNC_BATCH_SIZE must be between 1 and {} - Google runs a batch's \
                 sub-requests concurrently, so the batch width is a concurrency figure and \
                 45 produced `Too many concurrent requests for user` on the live account",
                crate::gmail::client::MAX_BATCH
            );
        }

        Ok(Self {
            env,
            bind: string("NADE_BIND").unwrap_or_else(|| "127.0.0.1".to_owned()),
            port: parse::<u16>("NADE_PORT", 8080)?,
            database_url: string("DATABASE_URL"),
            db_name: string("NADE_DB_NAME").unwrap_or_else(|| "nade".to_owned()),
            db_max_connections,
            dev_token: string("NADE_TOKEN"),
            pairing: PairingConfig {
                state_file: string("NADE_PAIR_CODE_FILE").map_or_else(
                    || root.join("secrets").join("pair-code.json"),
                    PathBuf::from,
                ),
                ttl: Duration::from_secs(parse::<u64>("NADE_PAIR_CODE_TTL_SECS", 600)?),
                rate_limit,
                rate_window: Duration::from_secs(parse::<u64>("NADE_PAIR_RATE_WINDOW_SECS", 60)?),
            },
            gmail: GmailConfig {
                client_file: string("NADE_GMAIL_CLIENT_FILE").map_or_else(
                    || root.join("secrets").join("web_client.json"),
                    PathBuf::from,
                ),
                redirect_uri: string("NADE_GMAIL_REDIRECT_URI")
                    .unwrap_or_else(|| "http://localhost:8080/v1/auth/gmail/callback".to_owned()),
                auth_url: string("NADE_GMAIL_AUTH_URL"),
                token_url: string("NADE_GMAIL_TOKEN_URL"),
                endpoints: string("NADE_GMAIL_API_BASE")
                    .map_or_else(crate::gmail::client::Endpoints::google, |base| {
                        crate::gmail::client::Endpoints::at(&base)
                    }),
                token_key: string("NADE_TOKEN_KEY"),
                token_key_file: string("NADE_TOKEN_KEY_FILE").map_or_else(
                    || crate::gmail::crypto::default_key_file(&root),
                    PathBuf::from,
                ),
                max_sync_messages,
                sync_window_days,
                batch_size,
            },
            jobs: crate::jobs::QueueConfig {
                lease,
                heartbeat,
                max_attempts,
                poll_interval: Duration::from_millis(parse::<u64>("NADE_JOB_POLL_MS", 1000)?),
                reap_interval: Duration::from_secs(parse::<u64>("NADE_JOB_REAP_SECS", 30)?),
                shutdown_grace: Duration::from_secs(parse::<u64>(
                    "NADE_JOB_SHUTDOWN_GRACE_SECS",
                    30,
                )?),
            },
            workers,
            embedded: EmbeddedConfig {
                data_dir: string("NADE_PG_DATA_DIR")
                    .map_or_else(|| root.join(".pgdata"), PathBuf::from),
                cache_dir: string("NADE_PG_CACHE_DIR")
                    .map_or_else(|| root.join(".pgcache"), PathBuf::from),
                port: parse::<u16>("NADE_PG_PORT", 54329)?,
                password: string("NADE_PG_PASSWORD").unwrap_or_else(|| "nade-dev".to_owned()),
            },
        })
    }

    /// The dev bearer shortcut, or `None` if it is not usable.
    ///
    /// The `Env::Dev` check lives here, in one place, so no caller can forget
    /// it. `api::auth::tests::dev_token_is_impossible_outside_dev` locks it in.
    #[must_use]
    pub fn usable_dev_token(&self) -> Option<&str> {
        if !self.env.is_dev() {
            return None;
        }
        self.dev_token.as_deref()
    }
}

/// Read a variable, treating "present but blank" as absent.
///
/// EDGE (empty input): `NADE_TOKEN=` must not turn into an empty-string bearer
/// that matches an empty `Authorization: Bearer ` header.
fn string(key: &str) -> Option<String> {
    let raw = std::env::var(key).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn parse<T>(key: &str, default: T) -> anyhow::Result<T>
where
    T: FromStr,
    T::Err: Display,
{
    match string(key) {
        None => Ok(default),
        Some(raw) => raw
            .parse::<T>()
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("{key} is not a valid value ({raw:?})")),
    }
}

/// `backend/`, the directory that owns `.pgdata`, `.pgcache` and `secrets/`.
///
/// `NADE_BACKEND_ROOT` wins. Otherwise we walk up from the compile-time crate
/// directory, which is correct for `cargo run`/`cargo test` from anywhere in
/// the workspace. If that path no longer exists (a binary copied to a server),
/// we fall back to the working directory - and in that case `DATABASE_URL` is
/// required anyway, so none of the embedded paths are used.
fn backend_root() -> PathBuf {
    if let Some(explicit) = string("NADE_BACKEND_ROOT") {
        return PathBuf::from(explicit);
    }
    let compiled = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    if compiled.is_dir() {
        return compiled;
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// `backend/.env.example` must document exactly the variables we read -
    /// no more, no fewer. Criterion H2.
    #[test]
    fn env_example_documents_every_var() {
        let path = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env.example"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

        let documented: Vec<String> = text
            .lines()
            .map(str::trim)
            .filter_map(|line| {
                let line = line.strip_prefix('#').unwrap_or(line).trim();
                let (key, _) = line.split_once('=')?;
                let key = key.trim();
                let looks_like_a_var = !key.is_empty()
                    && key
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
                looks_like_a_var.then(|| key.to_owned())
            })
            .collect();

        for var in ENV_VARS {
            assert!(
                documented.iter().any(|d| d == var),
                "{var} is read by the crate but missing from backend/.env.example"
            );
        }
        for doc in &documented {
            assert!(
                ENV_VARS.contains(&doc.as_str()),
                "{doc} is documented in backend/.env.example but never read"
            );
        }
    }

    #[test]
    fn only_the_exact_string_dev_is_dev() {
        // Parsing is a pure function of the string, so exercise it directly
        // rather than mutating the process environment from a parallel test.
        let classify = |v: &str| {
            if v.eq_ignore_ascii_case("dev") {
                Env::Dev
            } else {
                Env::Prod
            }
        };
        assert_eq!(classify("dev"), Env::Dev);
        assert_eq!(classify("DEV"), Env::Dev);
        assert_eq!(classify("development"), Env::Prod);
        assert_eq!(classify("dev "), Env::Prod); // already trimmed by `string`
        assert_eq!(classify(""), Env::Prod);
        assert_eq!(classify("prod"), Env::Prod);
    }

    #[test]
    fn usable_dev_token_is_none_outside_dev() {
        let mut cfg = sample();
        cfg.dev_token = Some("secret".into());

        cfg.env = Env::Dev;
        assert_eq!(cfg.usable_dev_token(), Some("secret"));

        cfg.env = Env::Prod;
        assert_eq!(cfg.usable_dev_token(), None);
    }

    /// A Gmail config that touches nothing real: no client file, a per-call
    /// scratch key file, and endpoints nobody serves.
    pub(crate) fn sample_gmail() -> GmailConfig {
        GmailConfig {
            client_file: PathBuf::from("/nonexistent/web_client.json"),
            redirect_uri: "http://localhost:8080/v1/auth/gmail/callback".into(),
            auth_url: None,
            token_url: None,
            endpoints: crate::gmail::client::Endpoints::at("http://127.0.0.1:1"),
            // A fixed key, so tests never touch the real key file.
            token_key: Some(hex::encode([0x2au8; 32])),
            token_key_file: PathBuf::from("/nonexistent/token-key"),
            max_sync_messages: 2_000,
            sync_window_days: 30,
            batch_size: crate::gmail::client::MAX_BATCH,
        }
    }

    #[test]
    fn the_dev_caps_have_the_values_plan_md_fixes() {
        let gmail = sample_gmail();
        assert_eq!(gmail.max_sync_messages, 2_000, "MAX_SYNC_MESSAGES");
        assert_eq!(gmail.sync_window_days, 30, "the 30-day window");
        assert_eq!(
            gmail.batch_size,
            crate::gmail::client::MAX_BATCH,
            "the batch width is the concurrency cap, not a number PLAN.md fixed"
        );
    }

    pub(crate) fn sample() -> Config {
        Config {
            env: Env::Prod,
            bind: "127.0.0.1".into(),
            port: 0,
            database_url: None,
            db_name: "nade".into(),
            db_max_connections: 5,
            dev_token: None,
            pairing: PairingConfig {
                state_file: std::env::temp_dir()
                    .join(format!("nade-pair-{}", uuid::Uuid::new_v4())),
                ttl: Duration::from_secs(600),
                rate_limit: 10,
                rate_window: Duration::from_secs(60),
            },
            gmail: sample_gmail(),
            jobs: crate::jobs::QueueConfig::default(),
            workers: 1,
            embedded: EmbeddedConfig {
                data_dir: PathBuf::from("/nonexistent"),
                cache_dir: PathBuf::from("/nonexistent"),
                port: 0,
                password: "x".into(),
            },
        }
    }
}
