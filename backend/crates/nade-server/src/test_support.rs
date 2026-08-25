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
        gmail: crate::config::tests::sample_gmail(),
        llm: crate::config::tests::sample_llm(),
        // A verifier with both claims set, pointed at a JWKS URL that refuses
        // instantly. A test that needs a real key set overrides it.
        push: crate::config::PushConfig {
            sa_email: Some("nade-push@example.iam.gserviceaccount.com".to_owned()),
            audience: Some("https://example.test/v1/webhooks/gmail".to_owned()),
            jwks_url: "http://127.0.0.1:1/certs".to_owned(),
            jwks_ttl: Duration::from_secs(3600),
            topic: Some("projects/p/topics/gmail-events".to_owned()),
        },
        schedule: crate::config::ScheduleConfig {
            tick: Duration::from_secs(60),
            watch_renew_after: Duration::from_secs(24 * 3600),
            poll_after: Duration::from_secs(30 * 60),
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

    /// A bearer token that reaches the protected routes. `NADE_TOKEN` is inert
    /// outside dev, so this pairs a device the way a real client would.
    pub async fn device_token(&self) -> String {
        let code = self.state.pairing.mint().await.unwrap();
        let response = post_json(
            self,
            "/v1/auth/pair",
            &serde_json::json!({ "code": code.code, "device_name": "test-device" }),
        )
        .await;
        response_json(response).await["token"]
            .as_str()
            .expect("pairing must return a token")
            .to_owned()
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

    /// Point the push verifier at a local JWK Set, and name the claims it
    /// should require.
    pub fn set_push(&mut self, jwks_url: &str, audience: &str, sa_email: &str) {
        self.config.push.jwks_url = jwks_url.to_owned();
        self.config.push.audience = Some(audience.to_owned());
        self.config.push.sa_email = Some(sa_email.to_owned());
        self.rebuild();
        // Rotation is asserted without sleeping through the production limit.
        self.state.push = std::sync::Arc::new(
            crate::api::webhooks::oidc::Verifier::new(
                self.state.gmail.http.clone(),
                self.config.push.clone(),
            )
            .with_min_refresh(Duration::ZERO),
        );
        self.router = api::router(self.state.clone());
    }

    /// Leave the webhook unconfigured, which must reject everything.
    pub fn clear_push_claims(&mut self) {
        self.config.push.audience = None;
        self.config.push.sa_email = None;
        self.rebuild();
    }

    /// Point the whole Gmail stack - REST and OAuth - at a local mock.
    pub fn set_gmail_base(&mut self, base: &str) {
        self.config.gmail.endpoints = crate::gmail::client::Endpoints::at(base);
        self.config.gmail.auth_url = Some(format!("{base}/auth"));
        self.config.gmail.token_url = Some(format!("{base}/token"));
        self.rebuild();
    }

    /// Point the model adapter at a `MockServer`.
    ///
    /// The counterpart of [`Self::set_gmail_base`], and the reason
    /// `NADE_LLM_API_BASE` exists at all: without it the adapter is untestable
    /// offline, and `backend/justfile` loads `backend/.env` into every `just
    /// test`, so a test that reached the real API would bill real money on
    /// whichever machine happens to hold a key.
    pub fn set_llm_base(&mut self, base: &str) {
        self.config.llm.api_base = base.trim_end_matches('/').to_owned();
        self.rebuild();
    }

    /// Set the per-account daily spend ceiling, in nano-USD.
    pub fn set_llm_ceiling_nano(&mut self, nano: i64) {
        self.config.llm.daily_ceiling_nano_usd = nano;
        self.rebuild();
    }

    /// Take the key away, so the "no key configured" path can be exercised.
    pub fn clear_llm_key(&mut self) {
        self.config.llm.api_key = None;
        self.rebuild();
    }

    /// Give the state a usable OAuth client without a `secrets/` file.
    pub fn set_gmail_client_file(&mut self, path: std::path::PathBuf) {
        self.config.gmail.client_file = path;
        self.rebuild();
    }

    /// The cipher the test config's fixed key produces, for seeding
    /// `gmail_tokens` rows the real `TokenStore` can read back.
    #[must_use]
    pub fn gmail_cipher(&self) -> crate::gmail::crypto::Cipher {
        crate::gmail::crypto::Cipher::resolve(
            self.config.gmail.token_key.as_deref(),
            &self.config.gmail.token_key_file,
        )
        .expect("the test config pins a fixed token key")
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
    post_json_as(app, path, body, None).await
}

/// `post_json`, with a bearer. Every P5 route needs one.
pub async fn post_json_as(
    app: &TestApp,
    path: &str,
    body: &Value,
    bearer: Option<&str>,
) -> Response<Body> {
    send(
        &app.router,
        "POST",
        path,
        bearer,
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

/// One scripted turn from the fake provider: prose, and nothing else.
///
/// The wire shape of an Anthropic answer, in one place. Three test modules had
/// it typed out — `agents::run`, `api::feed` and `agents::resume`, the last
/// hard-coding all three helpers inline — so a change to the fake provider's
/// shape was three edits, in a crate where `from_wire`'s strictness is itself
/// under test.
#[must_use]
pub fn text_turn(text: &str) -> wiremock::ResponseTemplate {
    wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "model": "claude-haiku-4-5-20251001",
        "stop_reason": "end_turn",
        "content": [{"type": "text", "text": text}],
        "usage": {"input_tokens": 100, "output_tokens": 20}
    }))
}

/// A turn that calls one tool, optionally after saying something.
///
/// `prose` is the more general of the two shapes that existed: `API.md` §6.1
/// says "`text` may be absent (a turn with no prose)", and both are real — the
/// card producer's fallback exists for the absent case, and `gate_context`
/// reads the prose when it is there.
#[must_use]
pub fn tool_turn(name: &str, input: Value, prose: Option<&str>) -> wiremock::ResponseTemplate {
    let mut content = Vec::new();
    if let Some(prose) = prose {
        content.push(serde_json::json!({"type": "text", "text": prose}));
    }
    content.push(serde_json::json!({
        "type": "tool_use", "id": "toolu_1", "name": name, "input": input
    }));
    wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "model": "claude-haiku-4-5-20251001",
        "stop_reason": "tool_use",
        "content": content,
        "usage": {"input_tokens": 100, "output_tokens": 20}
    }))
}

/// Answer with `first` once, then `rest` for everything after.
pub async fn script(
    server: &wiremock::MockServer,
    first: wiremock::ResponseTemplate,
    rest: wiremock::ResponseTemplate,
) {
    use wiremock::{
        matchers::{method, path},
        Mock,
    };
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(first)
        .up_to_n_times(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(rest)
        .mount(server)
        .await;
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
    ///
    /// `docs/contract/` is generated and keeps gaining files, so this asserts
    /// *structure* - every `.json` parses, every endpoint named in `API.md` §11
    /// has a fixture, every `.sse` looks like an SSE stream - rather than a file
    /// count that goes stale on the next regeneration.
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

        // API.md §11 names four `/ask` streams: answer, results, agent_draft and
        // error. Assert the shape of each rather than only the count, so a
        // renamed or truncated stream is caught too.
        let mut stream_names: Vec<String> = sse
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        stream_names.sort();
        for required in [
            "ask_agent_draft.sse",
            "ask_answer.sse",
            "ask_error.sse",
            "ask_results.sse",
        ] {
            assert!(
                stream_names.iter().any(|name| name == required),
                "{required} is missing; found {stream_names:?}"
            );
        }

        for path in &sse {
            let text = std::fs::read_to_string(path)
                .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
            let name = path.file_name().unwrap().to_string_lossy();
            assert!(
                text.starts_with("event: route"),
                "{name}: the first event is always `route` (API.md §4)"
            );
            for line in text.lines().filter(|line| line.starts_with("data: ")) {
                let payload = line.trim_start_matches("data: ");
                serde_json::from_str::<Value>(payload).unwrap_or_else(|error| {
                    panic!("{name}: `{payload}` is not valid JSON: {error}")
                });
            }
            let terminals = text
                .lines()
                .filter(|line| line.starts_with("event: done") || line.starts_with("event: error"));
            assert_eq!(
                terminals.count(),
                1,
                "{name}: exactly one terminal event, `done` OR `error`, never both"
            );
        }
    }
}
