//! Test harness: one embedded PostgreSQL per machine, one fresh database per
//! test, and a few request helpers.
//!
//! Isolation is by *database*, not by transaction, because the queue tests run
//! several connections against the same rows on purpose. Every test therefore
//! creates `nade_test_<uuid>` and migrates it; the next test binary sweeps
//! yesterday's leftovers away on start-up, so they never accumulate.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use axum::{
    body::Body,
    http::{header, Request, Response},
    Router,
};
use serde_json::Value;
use sqlx::{Connection, PgConnection, PgPool};
use tokio::sync::OnceCell;
use tower::ServiceExt;
use uuid::Uuid;

use crate::{
    api,
    config::{Config, EmbeddedConfig, Env, PairingConfig},
    db, embedded,
    jobs::QueueConfig,
    state::AppState,
};

/// Small enough that ~12 tests in parallel stay well inside the cluster's 200
/// connections, large enough for the 8-way claim race in `jobs::tests`.
const TEST_POOL_SIZE: u32 = 8;

struct Harness {
    admin_url: String,
    server: &'static embedded::Embedded,
}

static HARNESS: OnceCell<Harness> = OnceCell::const_new();

async fn harness() -> &'static Harness {
    HARNESS
        .get_or_init(|| async {
            init_tracing();
            let config = embedded_config();
            let server = embedded::shared(&config).await.unwrap_or_else(|error| {
                panic!("the test harness could not start PostgreSQL:\n{error:#}")
            });
            let admin_url = server.admin_url();
            sweep_old_test_databases(&admin_url).await;
            Harness { admin_url, server }
        })
        .await
}

fn embedded_config() -> EmbeddedConfig {
    // Same directories and port as `just run`, so tests and the dev server
    // share one postmaster instead of fighting over the data directory.
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    EmbeddedConfig {
        data_dir: env_path("NADE_PG_DATA_DIR").unwrap_or_else(|| root.join(".pgdata")),
        cache_dir: env_path("NADE_PG_CACHE_DIR").unwrap_or_else(|| root.join(".pgcache")),
        port: std::env::var("NADE_PG_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(54329),
        password: std::env::var("NADE_PG_PASSWORD").unwrap_or_else(|_| "nade-dev".to_owned()),
    }
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = fmt().with_env_filter(filter).with_test_writer().try_init();
}

/// Drop scratch databases left by earlier runs. Anything still in use (another
/// `cargo test` in flight) simply refuses to drop and is skipped.
async fn sweep_old_test_databases(admin_url: &str) {
    let Ok(mut conn) = PgConnection::connect(admin_url).await else {
        return;
    };
    let stale: Vec<String> =
        sqlx::query_scalar("select datname from pg_database where datname like 'nade\\_test\\_%'")
            .fetch_all(&mut conn)
            .await
            .unwrap_or_default();

    for name in &stale {
        let _ = sqlx::query(&format!("drop database if exists \"{name}\""))
            .execute(&mut conn)
            .await;
    }
    let _ = conn.close().await;
}

/// A private, migrated database for one test.
pub struct TestDb {
    pub pool: PgPool,
    pub name: String,
    pub url: String,
}

/// Create and migrate a database nobody else can see.
pub async fn test_db() -> TestDb {
    let harness = harness().await;
    let name = format!("nade_test_{}", Uuid::new_v4().simple());
    embedded::ensure_database(&harness.admin_url, &name)
        .await
        .unwrap_or_else(|error| panic!("creating {name}: {error:#}"));

    let url = harness.server.url(&name);
    let pool = db::pool(&url, TEST_POOL_SIZE)
        .await
        .unwrap_or_else(|error| panic!("connecting to {name}: {error:#}"));
    db::migrate(&pool)
        .await
        .unwrap_or_else(|error| panic!("migrating {name}: {error:#}"));

    TestDb { pool, name, url }
}

/// A config that touches nothing outside this test: its own pairing state file
/// and no usable embedded paths.
pub fn test_config(env: Env) -> Config {
    Config {
        env,
        bind: "127.0.0.1".to_owned(),
        port: 0,
        database_url: None,
        db_name: "nade".to_owned(),
        db_max_connections: TEST_POOL_SIZE,
        dev_token: None,
        pairing: PairingConfig {
            state_file: std::env::temp_dir().join(format!("nade-pair-{}.json", Uuid::new_v4())),
            ttl: Duration::from_secs(600),
            rate_limit: 100,
            rate_window: Duration::from_secs(60),
        },
        jobs: QueueConfig::default(),
        workers: 0,
        embedded: embedded_config(),
    }
}

/// A router wired to a private database.
pub struct TestApp {
    pub db: TestDb,
    pub config: Config,
    pub state: AppState,
    pub router: Router,
}

pub async fn test_app(env: Env) -> TestApp {
    let db = test_db().await;
    let config = test_config(env);
    let state = AppState::new(db.pool.clone(), config.clone());
    let router = api::router(state.clone());
    TestApp {
        db,
        config,
        state,
        router,
    }
}

impl TestApp {
    fn rebuild(&mut self) {
        self.state = AppState::new(self.db.pool.clone(), self.config.clone());
        self.router = api::router(self.state.clone());
    }

    pub fn set_dev_token(&mut self, token: Option<String>) {
        self.config.dev_token = token;
        self.rebuild();
    }

    pub fn set_pairing_ttl(&mut self, ttl: Duration) {
        self.config.pairing.ttl = ttl;
        self.rebuild();
    }

    pub fn set_pair_rate_limit(&mut self, max: u32, window: Duration) {
        self.config.pairing.rate_limit = max;
        self.config.pairing.rate_window = window;
        self.rebuild();
    }

    pub async fn raw_post(&self, path: &str, body: &str, content_type: &str) -> Response<Body> {
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(body.to_owned()))
            .unwrap();
        self.router.clone().oneshot(request).await.unwrap()
    }

    pub async fn raw_get(&self, path: &str, authorization: Option<&[u8]>) -> Response<Body> {
        let mut builder = Request::builder().method("GET").uri(path);
        if let Some(value) = authorization {
            builder = builder.header(
                header::AUTHORIZATION,
                axum::http::HeaderValue::from_bytes(value).unwrap(),
            );
        }
        self.router
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }
}

pub async fn send(
    router: &Router,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Option<(&str, String)>,
) -> Response<Body> {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(token) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let request = match body {
        Some((content_type, payload)) => builder
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(payload))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    router.clone().oneshot(request).await.unwrap()
}

pub async fn get(app: &TestApp, path: &str, bearer: Option<&str>) -> Response<Body> {
    send(&app.router, "GET", path, bearer, None).await
}

pub async fn post_json(app: &TestApp, path: &str, body: &Value) -> Response<Body> {
    send(
        &app.router,
        "POST",
        path,
        None,
        Some(("application/json", body.to_string())),
    )
    .await
}

pub async fn response_json(response: Response<Body>) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("reading the response body");
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "response was not JSON ({error}): {}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

/// `docs/contract/` - the canonical wire fixtures, shared with the iOS lane.
pub fn contract_dir() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../docs/contract"
    ))
}

/// A canonical wire fixture from `docs/contract/`.
pub fn fixture(name: &str) -> Value {
    let path = contract_dir().join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{} is not valid JSON: {error}", path.display()))
}

/// Poll `condition` until it holds, or fail the test.
pub async fn wait_for<F, Fut>(limit: Duration, mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + limit;
    loop {
        if condition().await {
            return;
        }
        assert!(Instant::now() < deadline, "condition never became true");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PLAN.md P1 [backend] bullet 2: the shared fixtures must be present and
    /// parseable. Both lanes decode these files, so a stray comma here is a
    /// cross-lane outage.
    #[test]
    fn every_contract_fixture_is_present_and_valid() {
        let dir = contract_dir();
        let mut json = Vec::new();
        let mut sse = Vec::new();

        for entry in std::fs::read_dir(&dir)
            .unwrap_or_else(|error| panic!("reading {}: {error}", dir.display()))
        {
            let path = entry.unwrap().path();
            match path.extension().and_then(|e| e.to_str()) {
                Some("json") => {
                    let name = path.file_name().unwrap().to_string_lossy().into_owned();
                    // `fixture` panics with the offending path on bad JSON.
                    let value = fixture(&name);
                    assert!(
                        value.is_object() || value.is_array(),
                        "{name} should be an object or array, got {value}"
                    );
                    json.push(name);
                }
                Some("sse") => sse.push(path),
                _ => {}
            }
        }

        // The README's table, minus the `error_*` group it summarises.
        for required in [
            "pair.json",
            "me.json",
            "mailboxes.json",
            "threads.json",
            "threads_last_page.json",
            "thread.json",
            "search.json",
            "drafts.json",
            "draft.json",
            "notes.json",
            "note.json",
            "agents.json",
            "agent.json",
            "run.json",
            "feed.json",
            "feed_item.json",
            "approve.json",
            "skip.json",
            "error_unauthorized.json",
        ] {
            assert!(json.iter().any(|n| n == required), "{required} is missing");
        }
        assert_eq!(sse.len(), 3, "expected the three /ask SSE fixtures");
    }
}
