//! `nade-server` - the binary.
//!
//! `nade-server` with no arguments serves the API. The handful of flags below
//! exist because `backend/justfile` needs them; there is no argument-parsing
//! crate here on purpose.

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use nade_server::{
    api,
    api::auth::pairing::{PairingCode, PairingStore},
    config::Config,
    db, embedded,
    jobs::{spawn_workers, Queue, Registry},
    state::AppState,
    VERSION,
};
use tokio::{net::TcpListener, sync::watch};

const HELP: &str = "\
nade-server - the NADE backend

USAGE:
    nade-server                 serve the API on NADE_BIND:NADE_PORT
    nade-server --print-pair-code   print the live pairing code, or mint a fresh one
    nade-server --migrate       create/upgrade the database, then exit
    nade-server --stop-db       stop the embedded dev PostgreSQL
    nade-server --version
    nade-server --help

Every environment variable is documented in backend/.env.example.
";

#[tokio::main]
async fn main() -> Result<()> {
    // A `backend/.env` (gitignored) overrides nothing that is already set.
    dotenvy::dotenv().ok();
    init_tracing();

    let config = Config::from_env()?;
    let mut args = std::env::args().skip(1);
    let command = args.next();
    if let Some(extra) = args.next() {
        bail!("unexpected argument {extra:?} - try --help");
    }

    match command.as_deref() {
        None => serve(config).await,
        Some("--print-pair-code") => print_pair_code(&config).await,
        Some("--migrate") => migrate(&config).await,
        Some("--stop-db") => {
            embedded::stop(&config.embedded).await?;
            println!("embedded postgres stopped");
            Ok(())
        }
        Some("--version" | "-V") => {
            println!("nade-server {VERSION}");
            Ok(())
        }
        Some("--help" | "-h") => {
            print!("{HELP}");
            Ok(())
        }
        Some(other) => bail!("unknown argument {other:?} - try --help"),
    }
}

async fn serve(config: Config) -> Result<()> {
    let database = db::connect(&config).await?;
    // `try_new`, not `new`: a malformed NADE_TOKEN_KEY is a misconfiguration the
    // process must refuse with an explanation rather than panic on.
    let state = AppState::try_new(database.pool.clone(), config.clone())
        .context("building the application state")?;

    // A fresh code on every boot, as PLAN.md §Auth bootstrap requires.
    let code = state
        .pairing
        .mint()
        .await
        .context("minting the startup pairing code")?;
    print_pairing_banner(&code, &config);

    let queue = Queue::new(database.pool.clone(), config.jobs.clone());
    let mut registry = Registry::new();
    nade_server::register_handlers(&mut registry, &state);
    let registry = Arc::new(registry);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let workers = if registry.is_empty() {
        tracing::info!("no job kinds registered yet (P1); not starting workers");
        Vec::new()
    } else {
        spawn_workers(&queue, &registry, config.workers, &shutdown_rx)
    };

    // Google's key set, fetched once so the first real push does not pay for
    // it. Fire-and-forget: a failure is a warning, never a boot failure - the
    // server starts with no network, exactly as it starts with no OAuth client.
    {
        let state = state.clone();
        tokio::spawn(async move { state.push.warm().await });
    }

    // The watch renewal and the polling fallback. A pure enqueuer: it asks the
    // database what is overdue and puts one deduplicated job on the queue.
    let scheduler = nade_server::sync::schedule::spawn(state.clone(), queue.clone());

    let address = format!("{}:{}", config.bind, config.port);
    let listener = TcpListener::bind(&address)
        .await
        .with_context(|| format!("binding {address}"))?;
    tracing::info!(%address, env = config.env.as_str(), version = VERSION, "nade-server listening");

    #[cfg(unix)]
    let sighup = tokio::spawn(regenerate_on_sighup(state.clone()));

    let result = axum::serve(listener, api::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await;

    tracing::info!("draining");
    #[cfg(unix)]
    sighup.abort();
    // Workers finish their in-flight job (or release its lease) and stop.
    let _ = shutdown_tx.send(true);
    // The scheduler holds nothing and writes nothing but an enqueue, so it is
    // aborted rather than drained; the workers own the in-flight jobs and are
    // given their grace period below.
    scheduler.abort();
    for worker in workers {
        let _ = worker.await;
    }
    database.pool.close().await;
    result.context("serving")?;
    tracing::info!("stopped");
    Ok(())
}

async fn migrate(config: &Config) -> Result<()> {
    let database = db::connect(config).await?;
    database.pool.close().await;
    println!("migrations applied");
    Ok(())
}

/// `just pair`. Reprints the live code if there is one; otherwise mints a fresh
/// one, which the running server picks up because the state file is the
/// authority on every pairing attempt.
async fn print_pair_code(config: &Config) -> Result<()> {
    let store = PairingStore::new(&config.pairing);
    let code = match store.current().await? {
        Some(live) => live,
        None => store
            .mint()
            .await
            .context("minting a replacement pairing code")?,
    };
    print_pairing_banner(&code, config);
    Ok(())
}

fn print_pairing_banner(code: &PairingCode, config: &Config) {
    let minutes = code.remaining(Utc::now()).as_secs().div_ceil(60);
    let host = if config.bind == "0.0.0.0" {
        "127.0.0.1"
    } else {
        &config.bind
    };
    // println!, not tracing: this must show up whatever RUST_LOG says.
    println!();
    println!("══════════════════════════════════════════════════════════════");
    println!("  NADE pairing code:  {}", code.code);
    println!("  single use, valid for {minutes} min");
    println!();
    println!(
        "  curl -sS -XPOST http://{host}:{}/v1/auth/pair \\",
        config.port
    );
    println!("    -H 'content-type: application/json' \\");
    println!(
        "    -d '{{\"code\":\"{}\",\"device_name\":\"my-iphone\"}}'",
        code.code
    );
    println!("══════════════════════════════════════════════════════════════");
    println!();
}

/// SIGHUP mints a new code and prints it - the in-process half of `just pair`.
#[cfg(unix)]
async fn regenerate_on_sighup(state: AppState) {
    use tokio::signal::unix::{signal, SignalKind};

    let Ok(mut hangups) = signal(SignalKind::hangup()) else {
        tracing::warn!("could not listen for SIGHUP; use `just pair` instead");
        return;
    };
    while hangups.recv().await.is_some() {
        match state.pairing.mint().await {
            Ok(code) => print_pairing_banner(&code, &state.config),
            Err(error) => tracing::error!(%error, "could not regenerate the pairing code"),
        }
    }
}

async fn shutdown_signal() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => tracing::info!("SIGINT received"),
        () = terminate => tracing::info!("SIGTERM received"),
    }
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn,tower_http=info"));
    fmt().with_env_filter(filter).init();
}
