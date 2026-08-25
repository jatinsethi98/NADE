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
    "NADE_PUSH_SA_EMAIL",
    "NADE_PUSH_AUDIENCE",
    "NADE_PUSH_JWKS_URL",
    "NADE_PUSH_JWKS_TTL_SECS",
    "NADE_PUSH_TOPIC",
    "NADE_SCHEDULER_TICK_SECS",
    "NADE_WATCH_RENEW_HOURS",
    "NADE_POLL_INTERVAL_MINS",
    "ANTHROPIC_API_KEY",
    "NADE_LLM_API_BASE",
    "NADE_LLM_MODEL",
    "NADE_LLM_COMPILE_MODEL",
    "NADE_LLM_TRIAGE_MODEL",
    "NADE_LLM_TIMEOUT_SECS",
    "NADE_LLM_MAX_ATTEMPTS",
    "NADE_LLM_DAILY_USD",
    "NADE_TRIAGE_DAILY_MAX",
    "NADE_RUN_MAX_STEPS",
    "NADE_RUN_TOKEN_BUDGET",
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

/// Gmail push: who is allowed to call the webhook, and what to watch.
///
/// Both claim checks are `Option`, and unset means **every push is rejected**.
/// The webhook is fail-closed on purpose: `aud` alone is forgeable, so a
/// half-configured server that accepted pushes would be worse than one that
/// accepted none.
#[derive(Debug, Clone)]
pub struct PushConfig {
    /// The OIDC `email` claim we require - the service account Pub/Sub mints
    /// its token as.
    pub sa_email: Option<String>,
    /// The OIDC `aud` claim, which must equal the audience set on the push
    /// subscription. With a quick tunnel this changes every session.
    pub audience: Option<String>,
    /// Google's JWK Set. Only the test suite overrides it.
    pub jwks_url: String,
    /// Fallback key-set lifetime when the response carries no `max-age`.
    pub jwks_ttl: Duration,
    /// The Pub/Sub topic `users.watch` registers against. Unset means no watch
    /// is registered and the poll is the only thing keeping mail current -
    /// which is exactly the dev-without-a-tunnel case.
    pub topic: Option<String>,
}

/// The model provider, the dev caps that bound a run, and the spend ceiling.
///
/// `api_key` is an `Option` for the same reason the Gmail client file is: a
/// server with no key must still boot and still serve mail. Only the agent
/// routes fail, and they fail with a message that says why.
#[derive(Clone)]
pub struct LlmConfig {
    /// Anthropic's key. `None` means every agent route answers
    /// `upstream_unavailable` rather than the process refusing to start.
    pub api_key: Option<String>,
    /// API root. The wiremock suite points this at a `MockServer`; nothing else
    /// ever sets it. See `llm::anthropic::Client::new` for the guard that stops
    /// a test reaching the real one.
    pub api_base: String,
    /// The model an agent run uses, unless the agent overrides it.
    pub model: String,
    /// The model `POST /agents` compiles a sentence with.
    pub compile_model: String,
    /// The model the mail trigger judges `spec.trigger.semantic` with.
    ///
    /// `None` falls back to [`Self::model`]. There is deliberately **no**
    /// per-agent override read here: `agents.triage_model` exists in the schema
    /// and no writer sets it — `PATCH /agents/{id}` accepts no such field
    /// (`API.md` §5) and the compiler does not emit one — so reading it would
    /// be a lookup that can only ever answer null.
    pub triage_model: Option<String>,
    /// Per request, applied to the request builder. `gmail::http_client()`
    /// bakes a 60 s client-wide timeout, so a value above that does nothing
    /// unless it is also set per request - which is why this is used that way.
    pub timeout: Duration,
    /// Attempts for a *retryable* status. 1 means "try once, never retry".
    pub max_attempts: u32,
    /// The daily spend ceiling, per account, in **nano-USD**.
    ///
    /// Money is an integer everywhere in this crate. The ceiling test is
    /// `spent >= ceiling` and it has to be exact at the boundary; a float
    /// would make the most important assertion in the phase a flaky one.
    pub daily_ceiling_nano_usd: i64,
    /// PLAN.md's <=20 triaged messages per agent per day. P5 enforces it; the
    /// ledger it reads lands here.
    pub triage_daily_max: i64,
    /// `EngineConfig::max_steps`.
    pub run_max_steps: u32,
    /// `EngineConfig::token_budget`.
    pub run_token_budget: u64,
}

impl std::fmt::Debug for LlmConfig {
    /// Hand-written, so the key cannot reach a log line.
    ///
    /// `AppState` already does this deliberately, for the same reason: one
    /// `tracing::debug!` that formats a struct is all it takes, and a derived
    /// `Debug` puts the secret in every one of them. `Config` embeds this, so
    /// it inherits the redaction.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmConfig")
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("api_base", &self.api_base)
            .field("model", &self.model)
            .field("compile_model", &self.compile_model)
            .field("triage_model", &self.triage_model)
            .field("timeout", &self.timeout)
            .field("max_attempts", &self.max_attempts)
            .field("daily_ceiling_nano_usd", &self.daily_ceiling_nano_usd)
            .field("triage_daily_max", &self.triage_daily_max)
            .field("run_max_steps", &self.run_max_steps)
            .field("run_token_budget", &self.run_token_budget)
            .finish()
    }
}

/// How often the background scheduler asks the database what is overdue.
#[derive(Debug, Clone)]
pub struct ScheduleConfig {
    pub tick: Duration,
    pub watch_renew_after: Duration,
    pub poll_after: Duration,
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
    pub push: PushConfig,
    pub schedule: ScheduleConfig,
    pub llm: LlmConfig,
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
        guard_pool_capacity(workers, db_max_connections)?;
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

        let scheduler_tick = Duration::from_secs(parse::<u64>("NADE_SCHEDULER_TICK_SECS", 60)?);
        let poll_after = Duration::from_secs(60 * parse::<u64>("NADE_POLL_INTERVAL_MINS", 30)?);
        let watch_renew_after =
            Duration::from_secs(3600 * parse::<u64>("NADE_WATCH_RENEW_HOURS", 24)?);
        if scheduler_tick.is_zero() || poll_after.is_zero() {
            bail!("NADE_SCHEDULER_TICK_SECS and NADE_POLL_INTERVAL_MINS must both be non-zero");
        }
        if scheduler_tick > poll_after {
            bail!(
                "NADE_SCHEDULER_TICK_SECS ({}s) is longer than NADE_POLL_INTERVAL_MINS ({}s), \
                 so the polling fallback would always be late",
                scheduler_tick.as_secs(),
                poll_after.as_secs()
            );
        }
        // Gmail's watch registration lasts seven days. Renewing more slowly than
        // that is not a preference, it is a guaranteed outage, so the process
        // refuses to start rather than discovering it a week later.
        if watch_renew_after >= Duration::from_secs(7 * 24 * 3600) {
            bail!(
                "NADE_WATCH_RENEW_HOURS ({}h) is at or past Gmail's 7-day watch lifetime; \
                 the registration would lapse before it was renewed",
                watch_renew_after.as_secs() / 3600
            );
        }

        // The model provider. Everything here has a working default except the
        // key, and an absent key is deliberately survivable (V4).
        let llm_max_attempts = parse::<u32>("NADE_LLM_MAX_ATTEMPTS", 3)?;
        if llm_max_attempts == 0 {
            bail!("NADE_LLM_MAX_ATTEMPTS must be at least 1 - 0 would never call the model");
        }
        let llm_timeout = Duration::from_secs(parse::<u64>("NADE_LLM_TIMEOUT_SECS", 60)?);
        if llm_timeout.is_zero() {
            bail!("NADE_LLM_TIMEOUT_SECS must be non-zero");
        }
        let daily_ceiling_nano_usd = nano_usd("NADE_LLM_DAILY_USD", 1_000_000_000)?;
        let triage_daily_max = parse::<i64>("NADE_TRIAGE_DAILY_MAX", 20)?;
        if triage_daily_max < 0 {
            bail!("NADE_TRIAGE_DAILY_MAX must not be negative");
        }
        let run_max_steps = parse::<u32>("NADE_RUN_MAX_STEPS", 12)?;
        if run_max_steps == 0 {
            bail!("NADE_RUN_MAX_STEPS must be at least 1");
        }
        let run_token_budget = parse::<u64>("NADE_RUN_TOKEN_BUDGET", 50_000)?;
        if run_token_budget == 0 {
            bail!("NADE_RUN_TOKEN_BUDGET must be at least 1");
        }
        // An empty string in the environment is not a model name. Treat it as
        // unset rather than sending "" to the provider and reading a 404 as an
        // outage.
        let llm_model =
            string("NADE_LLM_MODEL").unwrap_or_else(|| crate::llm::DEFAULT_MODEL.to_owned());
        let llm_compile_model =
            string("NADE_LLM_COMPILE_MODEL").unwrap_or_else(|| llm_model.clone());
        let llm_triage_model = string("NADE_LLM_TRIAGE_MODEL");
        let llm_api_base =
            string("NADE_LLM_API_BASE").unwrap_or_else(|| crate::llm::DEFAULT_API_BASE.to_owned());

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
            push: PushConfig {
                sa_email: string("NADE_PUSH_SA_EMAIL"),
                audience: string("NADE_PUSH_AUDIENCE"),
                jwks_url: string("NADE_PUSH_JWKS_URL")
                    .unwrap_or_else(|| "https://www.googleapis.com/oauth2/v3/certs".to_owned()),
                jwks_ttl: Duration::from_secs(parse::<u64>("NADE_PUSH_JWKS_TTL_SECS", 3600)?),
                topic: string("NADE_PUSH_TOPIC"),
            },
            schedule: ScheduleConfig {
                tick: scheduler_tick,
                watch_renew_after,
                poll_after,
            },
            llm: LlmConfig {
                api_key: string("ANTHROPIC_API_KEY"),
                api_base: llm_api_base,
                model: llm_model,
                compile_model: llm_compile_model,
                triage_model: llm_triage_model,
                timeout: llm_timeout,
                max_attempts: llm_max_attempts,
                daily_ceiling_nano_usd,
                triage_daily_max,
                run_max_steps,
                run_token_budget,
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

/// Refuse a pool the workers alone could drain.
///
/// Each running sync **pins** a pooled connection for the life of its account
/// lock (backend/DECISIONS.md D38), so `workers` syncs can hold `workers`
/// connections while every request handler still needs to acquire. With
/// `workers >= pool`, a busy queue starves the API outright - a hang with no
/// error anywhere - so the process refuses to start instead. The documented
/// rule of thumb is pool >= workers + 5.
fn guard_pool_capacity(workers: usize, db_max_connections: u32) -> anyhow::Result<()> {
    if workers >= db_max_connections as usize {
        bail!(
            "NADE_WORKERS ({workers}) must be smaller than NADE_DB_MAX_CONNECTIONS \
             ({db_max_connections}): each running sync pins a pooled connection for its \
             account lock, so this many workers can hold every connection and starve every \
             request handler. Keep the pool at least NADE_WORKERS + 5."
        );
    }
    Ok(())
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
/// Parse a decimal dollar amount into **nano-USD**, exactly.
///
/// Deliberately not `f64::from_str` followed by a multiply. The spend ceiling's
/// whole job is to be right at its boundary — `NADE_LLM_DAILY_USD=1.0` has to
/// mean 1_000_000_000 nano and not 999_999_999.99998 — so the digits are read
/// as integers and never pass through a binary float at all.
///
/// # Errors
/// Rejects a negative amount, a non-numeric one, more than nine fractional
/// digits (which nano-USD cannot represent), and an amount that would overflow.
fn nano_usd(key: &str, default: i64) -> anyhow::Result<i64> {
    const NANO: i64 = 1_000_000_000;
    let Some(raw) = string(key) else {
        return Ok(default);
    };
    let raw = raw.trim();
    // EDGE: empty input. A blank value is "unset", not "free".
    if raw.is_empty() {
        return Ok(default);
    }
    if raw.starts_with('-') {
        bail!("{key} must not be negative (got {raw:?})");
    }
    let (whole, frac) = match raw.split_once('.') {
        Some((w, f)) => (w, f),
        None => (raw, ""),
    };
    // `split_once` leaves an empty side for "1." or ".5"; both are accepted,
    // but a wholly empty number ("." alone) is not.
    if whole.is_empty() && frac.is_empty() {
        bail!("{key} is not a number (got {raw:?})");
    }
    if !whole.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
        bail!("{key} is not a decimal number (got {raw:?})");
    }
    if frac.len() > 9 {
        bail!(
            "{key} has {} fractional digits; nano-USD holds at most 9 (got {raw:?})",
            frac.len()
        );
    }
    let whole: i64 = if whole.is_empty() {
        0
    } else {
        whole
            .parse()
            .with_context(|| format!("{key}: {raw:?} is too large"))?
    };
    // Right-pad so "5" means 500_000_000 nano and not 5. `{:0<9}` is the
    // padding; the digits and the <=9 length are already proven above, so the
    // parse cannot fail and the empty case cannot arise - the loop this
    // replaced ended in an `is_empty()` branch it had just made unreachable.
    let frac: i64 = format!("{frac:0<9}").parse().unwrap_or(0);
    whole
        .checked_mul(NANO)
        .and_then(|w| w.checked_add(frac))
        .with_context(|| format!("{key}: {raw:?} overflows"))
}

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
    use std::collections::HashMap;

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

    /// Every value `.env.example` actually sets must be one the server would
    /// accept. Criterion U2.
    ///
    /// `env_example_documents_every_var` compares **names**, which is why
    /// `NADE_SYNC_BATCH_SIZE=45` survived in the example for a whole phase
    /// after `MAX_BATCH` became 10: copying the documented file to
    /// `backend/.env` verbatim made the server refuse to boot, and nothing
    /// failed. This checks the values that carry a hard bound, so the next
    /// stale default cannot land the same way.
    #[test]
    fn every_documented_value_is_one_the_server_would_boot_with() {
        let path = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env.example"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

        // Only the lines that are actually set. A commented line is an
        // illustration, not a value the server will ever read.
        let mut set: HashMap<&str, &str> = HashMap::new();
        for line in text.lines().map(str::trim) {
            if line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                set.insert(key.trim(), value.trim());
            }
        }

        // A malformed value must FAIL, not vanish. `.parse().ok()` would turn
        // `NADE_SYNC_BATCH_SIZE=ten` into `None` and skip every range assertion
        // below, so the test would pass on a file the server refuses to read.
        let number = |key: &str| -> Option<u64> {
            set.get(key).map(|raw| {
                raw.parse().unwrap_or_else(|_| {
                    panic!(
                        "backend/.env.example sets {key}={raw:?}, which is not a number - \
                         Config::from_env would refuse to start"
                    )
                })
            })
        };

        if let Some(batch) = number("NADE_SYNC_BATCH_SIZE") {
            let max = u64::try_from(crate::gmail::client::MAX_BATCH).unwrap();
            assert!(
                (1..=max).contains(&batch),
                "backend/.env.example sets NADE_SYNC_BATCH_SIZE={batch}, but the server \
                 refuses to start outside 1..={max} - copying the example would brick it"
            );
        }
        for key in ["NADE_MAX_SYNC_MESSAGES", "NADE_SYNC_WINDOW_DAYS"] {
            if let Some(value) = number(key) {
                assert!(
                    value >= 1,
                    "backend/.env.example sets {key}={value}, and the server requires at least 1"
                );
            }
        }
        if let (Some(heartbeat), Some(lease)) = (
            number("NADE_JOB_HEARTBEAT_SECS"),
            number("NADE_JOB_LEASE_SECS"),
        ) {
            assert!(
                heartbeat > 0 && heartbeat < lease,
                "backend/.env.example sets a heartbeat of {heartbeat}s against a {lease}s \
                 lease; the server refuses to start unless the heartbeat is shorter"
            );
        }

        // The check must be able to fail. If none of the bounded variables is
        // set, everything above is vacuous and this test proves nothing.
        assert!(
            set.contains_key("NADE_SYNC_BATCH_SIZE"),
            "no bounded variable is set in backend/.env.example, so this test checked nothing"
        );
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

    /// D38: a running sync pins a connection, so the pool must outnumber the
    /// workers - checked from both sides, boundary included.
    #[test]
    fn the_config_refuses_a_pool_the_workers_could_drain() {
        // The defaults (2 workers, 10 connections) must boot.
        assert!(guard_pool_capacity(2, 10).is_ok());
        // Zero workers is a valid quiet server, whatever the pool.
        assert!(guard_pool_capacity(0, 1).is_ok());
        // Equality is refused: `workers` pinned connections leave zero for the
        // API, and `>` would let exactly that configuration boot.
        let error = guard_pool_capacity(10, 10).unwrap_err().to_string();
        assert!(error.contains("NADE_WORKERS"), "{error}");
        assert!(error.contains("NADE_DB_MAX_CONNECTIONS"), "{error}");
        assert!(guard_pool_capacity(11, 10).is_err());
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

    /// An LLM config that cannot reach the real provider.
    ///
    /// The base URL is a port that refuses instantly, the same trick the JWKS
    /// URL above uses. A test that wants a fake provider calls
    /// `TestApp::set_llm_base`; a test that forgets would get a connection
    /// error rather than a bill, and `guard_against_live_calls_in_tests`
    /// catches the case where the real URL is reached for anyway.
    pub(crate) fn sample_llm() -> LlmConfig {
        LlmConfig {
            api_key: Some("test-key-not-a-real-one".to_owned()),
            api_base: "http://127.0.0.1:1".to_owned(),
            model: "claude-haiku-4-5".to_owned(),
            compile_model: "claude-haiku-4-5".to_owned(),
            triage_model: None,
            timeout: Duration::from_secs(5),
            // One attempt: a test that exercises the retry policy sets its own
            // value, and every other test would otherwise pay the backoff.
            max_attempts: 1,
            daily_ceiling_nano_usd: 1_000_000_000,
            triage_daily_max: 20,
            run_max_steps: 12,
            run_token_budget: 50_000,
        }
    }

    /// `nano_usd` reads the environment, so these have to serialise against
    /// each other. Same guard the rest of this module's env tests use.
    fn with_var<T>(key: &str, value: Option<&str>, body: impl FnOnce() -> T) -> T {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = std::env::var(key).ok();
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        let out = body();
        match previous {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        out
    }

    const K: &str = "NADE_TEST_NANO_USD";

    fn nano(value: &str) -> anyhow::Result<i64> {
        with_var(K, Some(value), || nano_usd(K, -1))
    }

    #[test]
    fn a_dollar_is_exactly_a_billion_nano() {
        // The whole reason this parser exists: `1.0` must not become
        // 999_999_999 by way of a binary float.
        assert_eq!(nano("1.0").unwrap(), 1_000_000_000);
        assert_eq!(nano("1").unwrap(), 1_000_000_000);
        assert_eq!(nano("1.").unwrap(), 1_000_000_000);
    }

    #[test]
    fn fractions_scale_by_position_not_by_digit_count() {
        // "5" after the point is five *tenths*, not five nano.
        assert_eq!(nano("0.5").unwrap(), 500_000_000);
        assert_eq!(nano(".5").unwrap(), 500_000_000);
        assert_eq!(nano("0.000000001").unwrap(), 1);
        assert_eq!(nano("2.25").unwrap(), 2_250_000_000);
    }

    #[test]
    fn zero_is_a_legal_ceiling_and_means_no_calls_at_all() {
        assert_eq!(nano("0").unwrap(), 0);
        assert_eq!(nano("0.0").unwrap(), 0);
    }

    #[test]
    fn an_unset_or_blank_value_falls_back_to_the_default() {
        assert_eq!(with_var(K, None, || nano_usd(K, 7)).unwrap(), 7);
        // EDGE: empty input. A blank value is "unset", not "free".
        assert_eq!(nano("").unwrap(), -1);
        assert_eq!(nano("   ").unwrap(), -1);
    }

    #[test]
    fn a_negative_ceiling_is_refused_rather_than_silently_blocking_everything() {
        assert!(nano("-1").is_err());
        assert!(nano("-0.5").is_err());
    }

    #[test]
    fn nonsense_is_refused_at_boot_not_at_the_first_model_call() {
        for bad in ["abc", "1.2.3", "1e9", "$1", "1,0", ".", "1.0abc"] {
            assert!(nano(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn more_precision_than_nano_is_refused_rather_than_rounded() {
        // Ten decimals cannot be represented. Silently truncating would make
        // the configured ceiling and the enforced ceiling different numbers.
        assert!(nano("0.0000000001").is_err());
        assert_eq!(nano("0.999999999").unwrap(), 999_999_999);
    }

    #[test]
    fn an_absurd_amount_is_refused_rather_than_wrapping_negative() {
        // A wrap would produce a negative ceiling, which blocks every call -
        // the failure would look like a bug in the ceiling, not in the config.
        assert!(nano("99999999999999999999").is_err());
        assert!(nano("9223372036.854775808").is_err());
    }

    #[test]
    fn unicode_digits_are_not_digits() {
        // EDGE: unicode. Arabic-Indic digits parse as numbers in some
        // languages; here they must not.
        assert!(nano("\u{0661}").is_err());
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
            llm: sample_llm(),
            push: PushConfig {
                sa_email: Some("nade-push@example.iam.gserviceaccount.com".to_owned()),
                audience: Some("https://example.test/v1/webhooks/gmail".to_owned()),
                jwks_url: "http://127.0.0.1:1/certs".to_owned(),
                jwks_ttl: Duration::from_secs(3600),
                topic: Some("projects/p/topics/gmail-events".to_owned()),
            },
            schedule: ScheduleConfig {
                tick: Duration::from_secs(60),
                watch_renew_after: Duration::from_secs(24 * 3600),
                poll_after: Duration::from_secs(30 * 60),
            },
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
