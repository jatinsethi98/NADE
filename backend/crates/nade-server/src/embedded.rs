//! Docker-free PostgreSQL for dev and tests (PLAN.md §Recorded deviations 5).
//!
//! One postmaster per machine, living in `backend/.pgdata`, with its binaries
//! cached in `backend/.pgcache`. Both directories are already gitignored.
//!
//! The server is deliberately **left running** when our process exits: the dev
//! server and every `cargo test` run reuse it, which turns a ~20 s cold boot
//! into a ~30 ms probe. `just db-stop` (`nade-server --stop-db`) shuts it down.

use std::{path::Path, time::Duration};

use anyhow::{anyhow, bail, Context, Result};
use postgresql_embedded::{PostgreSQL, Settings};
use sqlx::{postgres::PgConnectOptions, Connection, PgConnection};
use tokio::sync::OnceCell;

use crate::config::EmbeddedConfig;

/// The bootstrap database every embedded cluster ships with.
const BOOTSTRAP_DB: &str = "postgres";

/// How long we keep re-probing after a failed `start()`, to absorb the case
/// where a sibling process won the race and is mid-boot.
const RACE_PATIENCE: Duration = Duration::from_secs(20);

/// A reachable embedded PostgreSQL.
#[derive(Debug, Clone)]
pub struct Embedded {
    settings: Settings,
}

impl Embedded {
    /// Connection URL for a database inside this cluster.
    #[must_use]
    pub fn url(&self, database: &str) -> String {
        self.settings.url(database)
    }

    /// Connection URL for the bootstrap database, used to `create database`.
    #[must_use]
    pub fn admin_url(&self) -> String {
        self.url(BOOTSTRAP_DB)
    }

    #[must_use]
    pub fn port(&self) -> u16 {
        self.settings.port
    }
}

static SHARED: OnceCell<Embedded> = OnceCell::const_new();

/// The process-wide embedded cluster, booting it on first call.
///
/// Concurrent callers inside one process are serialised by [`OnceCell`];
/// concurrent *processes* are handled by the probe-start-reprobe dance in
/// [`boot`].
///
/// # Errors
/// Returns an error if the binaries cannot be installed or the server cannot be
/// reached. The message says which directory it was writing to and what to do.
pub async fn shared(cfg: &EmbeddedConfig) -> Result<&'static Embedded> {
    SHARED.get_or_try_init(|| boot(cfg)).await
}

/// Keeps the started server alive for the life of the process. Statics are
/// never dropped, and `PostgreSQL`'s `Drop` is what would stop the postmaster.
static STARTED: std::sync::OnceLock<PostgreSQL> = std::sync::OnceLock::new();

async fn boot(cfg: &EmbeddedConfig) -> Result<Embedded> {
    std::fs::create_dir_all(&cfg.cache_dir)
        .with_context(|| format!("creating {}", cfg.cache_dir.display()))?;
    if let Some(parent) = cfg.data_dir.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    let settings = settings_for(cfg);
    let admin_url = settings.url(BOOTSTRAP_DB);

    // Fast path: somebody (a previous `cargo test`, or `just run`) already has
    // it up. Reuse rather than fight over the data directory.
    if probe(&admin_url).await {
        tracing::debug!(
            port = settings.port,
            "reusing the running embedded postgres"
        );
        return Ok(Embedded { settings });
    }

    // The password file is written *before* setup so a fresh `initdb` bakes in
    // the password our URL uses. On an existing cluster it is already correct.
    write_password_file(&settings)?;

    let mut server = PostgreSQL::new(settings.clone());
    let start_error = match server.setup().await {
        Err(error) => {
            return Err(anyhow!("{error}").context(download_hint(cfg)));
        }
        Ok(()) => server.start().await.err(),
    };

    // `pg_ctl start` fails when another process just started the same data
    // directory. That is a success for us as long as the server answers.
    let deadline = std::time::Instant::now() + RACE_PATIENCE;
    loop {
        // `setup()` resolves the versioned installation directory, so re-read
        // the settings rather than trusting the ones we passed in.
        let settings = server.settings().clone();
        if probe(&settings.url(BOOTSTRAP_DB)).await {
            if start_error.is_none() {
                tracing::info!(
                    port = settings.port,
                    data_dir = %settings.data_dir.display(),
                    "started embedded postgres"
                );
                let _ = STARTED.set(server);
            }
            return Ok(Embedded { settings });
        }
        if std::time::Instant::now() >= deadline {
            let detail = start_error
                .map_or_else(|| "the server never answered".to_owned(), |e| e.to_string());
            bail!(
                "embedded postgres at 127.0.0.1:{} never became reachable: {detail}\n\
                 - if you changed NADE_PG_PASSWORD after the cluster was created, either put it \
                   back or delete {} and let it re-initialise\n\
                 - if something else owns that port, set NADE_PG_PORT\n\
                 - to skip the embedded server entirely, set DATABASE_URL",
                settings.port,
                cfg.data_dir.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn settings_for(cfg: &EmbeddedConfig) -> Settings {
    let mut settings = Settings::new();
    settings.installation_dir.clone_from(&cfg.cache_dir);
    settings.data_dir.clone_from(&cfg.data_dir);
    settings.password_file = cfg.cache_dir.join(".pgpass");
    settings.host = "127.0.0.1".to_owned();
    settings.port = cfg.port;
    settings.password.clone_from(&cfg.password);
    // Never delete the data directory on drop - this cluster is meant to
    // outlive the process.
    settings.temporary = false;
    // The default 5 s is far too short for `initdb` on a cold machine.
    settings.timeout = Some(Duration::from_secs(300));
    // Room for `cargo test`'s default parallelism: every test opens its own
    // database with its own small pool.
    settings
        .configuration
        .insert("max_connections".into(), "200".into());
    // This cluster holds nothing but dev scratch and per-test databases, so
    // trade crash durability for speed.
    settings.configuration.insert("fsync".into(), "off".into());
    settings
        .configuration
        .insert("synchronous_commit".into(), "off".into());
    settings
        .configuration
        .insert("full_page_writes".into(), "off".into());
    settings
}

fn write_password_file(settings: &Settings) -> Result<()> {
    if let Some(parent) = settings.password_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&settings.password_file, &settings.password)
        .with_context(|| format!("writing {}", settings.password_file.display()))?;
    restrict(&settings.password_file);
    Ok(())
}

fn restrict(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn download_hint(cfg: &EmbeddedConfig) -> String {
    format!(
        "could not install the embedded PostgreSQL binaries into {}.\n\
         The first boot downloads a ~30 MB PostgreSQL archive from \
         https://github.com/theseus-rs/postgresql-binaries - check network and proxy access, \
         then run again (the download is cached, so retries are cheap).\n\
         To use a PostgreSQL you already have instead, set DATABASE_URL and the embedded server \
         is never touched.",
        cfg.cache_dir.display()
    )
}

/// Can we open a connection and round-trip a query right now?
async fn probe(url: &str) -> bool {
    let Ok(options) = url.parse::<PgConnectOptions>() else {
        return false;
    };
    let attempt = async {
        let mut conn = PgConnection::connect_with(&options).await?;
        let one: i32 = sqlx::query_scalar("select 1").fetch_one(&mut conn).await?;
        conn.close().await?;
        Ok::<bool, sqlx::Error>(one == 1)
    };
    matches!(
        tokio::time::timeout(Duration::from_secs(5), attempt).await,
        Ok(Ok(true))
    )
}

/// `create database` if it is not there yet.
///
/// # Errors
/// Returns an error if the admin connection fails or the statement is rejected
/// for anything other than "it already exists".
pub async fn ensure_database(admin_url: &str, name: &str) -> Result<()> {
    let mut conn = PgConnection::connect(admin_url)
        .await
        .context("connecting to the embedded postgres bootstrap database")?;

    let exists: Option<i32> = sqlx::query_scalar("select 1 from pg_database where datname = $1")
        .bind(name)
        .fetch_optional(&mut conn)
        .await?;

    if exists.is_none() {
        // EDGE (concurrent workers racing): two processes can both see it
        // missing. 42P04 is `duplicate_database` - the other one won, which is
        // exactly the outcome we wanted.
        if let Err(error) = sqlx::query(&format!("create database {}", quote_ident(name)))
            .execute(&mut conn)
            .await
        {
            if !is_duplicate_database(&error) {
                conn.close().await.ok();
                return Err(error).with_context(|| format!("creating database {name}"));
            }
        }
    }

    conn.close().await.ok();
    Ok(())
}

/// `drop database if exists`, used by the test harness to sweep old scratch
/// databases. Databases with live connections are left alone.
///
/// # Errors
/// Returns an error only if the admin connection itself fails.
pub async fn drop_database(admin_url: &str, name: &str) -> Result<()> {
    let mut conn = PgConnection::connect(admin_url).await?;
    let _ = sqlx::query(&format!("drop database if exists {}", quote_ident(name)))
        .execute(&mut conn)
        .await;
    conn.close().await.ok();
    Ok(())
}

fn is_duplicate_database(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "42P04")
}

/// Quote an identifier the way PostgreSQL does. Database names here come from
/// config or a uuid, but never interpolate an unquoted identifier.
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Stop the shared cluster (`just db-stop`).
///
/// # Errors
/// Returns an error if the binaries cannot be located or `pg_ctl stop` fails.
pub async fn stop(cfg: &EmbeddedConfig) -> Result<()> {
    let mut server = PostgreSQL::new(settings_for(cfg));
    // Resolves the versioned installation directory so `pg_ctl` can be found;
    // no network and no `initdb` for an existing cluster.
    server
        .setup()
        .await
        .map_err(|e| anyhow!("{e}"))
        .context("locating the embedded postgres installation")?;
    server
        .stop()
        .await
        .map_err(|e| anyhow!("{e}"))
        .context("stopping the embedded postgres")
}

#[cfg(test)]
mod tests {
    use super::quote_ident;

    #[test]
    fn identifiers_are_quoted_and_escaped() {
        assert_eq!(quote_ident("nade"), "\"nade\"");
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
        // EDGE (unicode): non-ASCII identifiers survive quoting unharmed.
        assert_eq!(quote_ident("メール"), "\"メール\"");
    }
}
