//! Connection pool and migrations.
//!
//! `DATABASE_URL` wins. With it unset and `NADE_ENV=dev` we boot the embedded
//! server (see [`crate::embedded`]); with it unset anywhere else we refuse to
//! start, loudly. There is no third path, and in particular no silent embedded
//! database in production.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use sqlx::{migrate::Migrator, postgres::PgPoolOptions, PgPool};

use crate::{config::Config, embedded};

/// The migrations are embedded at compile time, so `cargo build` never needs a
/// database. (`sqlx::migrate!` reads .sql files; it is not a query macro - see
/// backend/DECISIONS.md D1.)
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// A ready-to-use, migrated database.
#[derive(Debug, Clone)]
pub struct Db {
    pub pool: PgPool,
    pub url: String,
}

/// Resolve the database, connect, and migrate.
///
/// # Errors
/// Returns an error if the URL cannot be resolved, the server is unreachable,
/// or a migration fails.
pub async fn connect(config: &Config) -> Result<Db> {
    let url = resolve_url(config).await?;
    let pool = pool(&url, config.db_max_connections).await?;
    migrate(&pool).await?;
    Ok(Db { pool, url })
}

async fn resolve_url(config: &Config) -> Result<String> {
    if let Some(url) = &config.database_url {
        tracing::info!("using DATABASE_URL");
        return Ok(url.clone());
    }

    if !config.env.is_dev() {
        bail!(
            "DATABASE_URL is unset and NADE_ENV is `{}`, not `dev`.\n\
             The embedded PostgreSQL is a development convenience and is never started outside \
             dev. Set DATABASE_URL, or set NADE_ENV=dev if this really is a dev machine.",
            config.env.as_str()
        );
    }

    let server = embedded::shared(&config.embedded).await?;
    embedded::ensure_database(&server.admin_url(), &config.db_name).await?;
    let url = server.url(&config.db_name);
    // The password here is the local dev one from NADE_PG_PASSWORD; printing
    // the whole URL is the point - you paste it straight into psql. This branch
    // is unreachable outside NADE_ENV=dev.
    tracing::info!(%url, "dev database ready");
    Ok(url)
}

/// Build a pool. Lazy is deliberate: `/v1/healthz` should answer 503 rather
/// than the process failing to start when the database is briefly away.
///
/// # Errors
/// Returns an error only if the URL itself is malformed.
pub async fn pool(url: &str, max_connections: u32) -> Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .min_connections(0)
        .acquire_timeout(Duration::from_secs(10))
        .connect(url)
        .await
        .with_context(|| "connecting to postgres".to_owned())
}

/// Apply pending migrations. Already-applied versions are skipped, so this is
/// safe to run on every boot and safe to run twice.
///
/// # Errors
/// Returns an error if a migration fails or a checksum no longer matches.
pub async fn migrate(pool: &PgPool) -> Result<()> {
    MIGRATOR
        .run(pool)
        .await
        .context("applying migrations from crates/nade-server/migrations")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::{
        config::{tests::sample, Env},
        test_support::test_db,
    };

    /// Criterion B1 - every table PLAN.md §Postgres schema names.
    #[tokio::test]
    async fn migration_creates_every_planned_table() {
        let db = test_db().await;
        let mut present: Vec<String> = sqlx::query_scalar(
            "select table_name from information_schema.tables \
             where table_schema = 'public' and table_name <> '_sqlx_migrations' \
             order by table_name",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        present.sort();

        let mut expected = vec![
            "accounts",
            "agent_runs",
            "agents",
            "attachments",
            "audit_log",
            "devices",
            "drafts",
            "feed_items",
            "gmail_tokens",
            "jobs",
            "labels",
            "messages",
            "notes",
            "run_journal",
            "settings",
            "sync_state",
            // P2 rollups - backend/DECISIONS.md D13.
            "thread_labels",
            "threads",
        ];
        expected.sort_unstable();

        assert_eq!(present, expected);
    }

    /// Criterion B2, inverted by `docs/SEARCH.md`: **nothing maintains a second
    /// index.**
    ///
    /// `messages` used to carry a generated `tsvector` and a GIN index over it.
    /// It indexed the 30-day sync window - ~500 of 63,866 messages, 0.78% - so
    /// a search for anything older returned an empty result indistinguishable
    /// from "no such mail exists". Gmail is the index now.
    ///
    /// Checked from both directions, because either alone can be defeated: the
    /// live schema must hold no tsvector, and no source file may name one. A
    /// column added by a future migration fails the first half; a query built
    /// at runtime against a column that does not exist fails the second before
    /// it can fail in production.
    #[tokio::test]
    async fn nothing_maintains_a_second_index() {
        let db = test_db().await;

        let columns: Vec<(String, String)> = sqlx::query_as(
            "select table_name, column_name from information_schema.columns \
             where table_schema = 'public' and data_type = 'tsvector'",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert!(
            columns.is_empty(),
            "docs/SEARCH.md: nothing maintains a second index, but {columns:?} exist"
        );

        let text_indexes: Vec<String> = sqlx::query_scalar(
            "select indexdef from pg_indexes \
             where schemaname = 'public' and indexdef ilike '%tsvector%'",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert!(
            text_indexes.is_empty(),
            "a full-text index survived the migration: {text_indexes:?}"
        );

        // And the source. Assembled at runtime so this test is not its own
        // offender, exactly like `no_compile_time_query_macros`.
        let banned = [
            concat!("ts", "vector"),
            concat!("to_ts", "vector"),
            concat!("websearch_to_ts", "query"),
            concat!("plainto_ts", "query"),
            concat!("phraseto_ts", "query"),
            concat!("ts_", "rank"),
        ];
        let roots = [
            PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src")),
            PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations")),
        ];
        let mut offenders = Vec::new();
        let mut stack: Vec<PathBuf> = roots.into_iter().collect();
        let here = file!();

        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                match path.extension().and_then(|e| e.to_str()) {
                    Some("rs" | "sql") => {}
                    _ => continue,
                }
                if path.ends_with(here) {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                for (number, line) in text.lines().enumerate() {
                    // A comment cannot build an index. Explaining *why* the
                    // column is gone - which is the most useful thing either
                    // file can say to whoever reads it next - must not trip the
                    // check that keeps it gone.
                    let trimmed = line.trim_start();
                    if trimmed.starts_with("//") || trimmed.starts_with("--") {
                        continue;
                    }
                    if banned.iter().any(|needle| line.contains(needle)) {
                        offenders.push(format!("{}:{}: {trimmed}", path.display(), number + 1));
                    }
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "docs/SEARCH.md: Gmail is the index and nothing here builds a second one:\n{}",
            offenders.join("\n")
        );
    }

    /// Criterion B3.
    #[tokio::test]
    async fn required_indexes_exist() {
        let db = test_db().await;
        let definitions: Vec<(String, String)> = sqlx::query_as(
            "select indexname, indexdef from pg_indexes where schemaname = 'public'",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        let find = |name: &str| {
            definitions
                .iter()
                .find(|(n, _)| n == name)
                .unwrap_or_else(|| panic!("index {name} is missing"))
                .1
                .clone()
        };

        assert!(find("messages_account_ts_idx").contains("internal_ts DESC"));
        assert!(find("messages_account_thread_idx").contains("thread_id"));
        // `messages_fts_idx` is deliberately absent - see
        // `nothing_maintains_a_second_index`. `label_ids` keeps its GIN index:
        // that is a plain SQL predicate over the cache, not a text index.
        assert!(find("messages_label_ids_idx").contains("USING gin"));
        assert!(find("agent_runs_status_wake_idx").contains("wake_at"));
        assert!(find("feed_items_account_status_created_idx").contains("created_at DESC"));

        // Criterion I6 - the keyset walk `GET /mailboxes/{id}/threads` performs.
        let keyset = find("thread_labels_keyset_idx");
        for fragment in ["account_id", "label_id", "last_ts DESC", "thread_id DESC"] {
            assert!(keyset.contains(fragment), "{keyset}");
        }
        assert!(find("threads_keyset_idx").contains("last_ts DESC"));

        let jobs_index = find("jobs_ready_idx");
        assert!(jobs_index.contains("run_after"), "{jobs_index}");
        assert!(jobs_index.contains("done_at IS NULL"), "{jobs_index}");
        assert!(jobs_index.contains("dead_at IS NULL"), "{jobs_index}");

        // `dedupe_key text unique` gives us the unique index the plan asks for.
        assert!(
            definitions
                .iter()
                .any(|(_, def)| def.contains("UNIQUE") && def.contains("dedupe_key")),
            "agent_runs.dedupe_key must be unique"
        );
    }

    /// Criterion B4 - the plan's `check` constraints are real.
    #[tokio::test]
    async fn status_check_constraints_reject_bad_values() {
        let db = test_db().await;
        let account: uuid::Uuid = sqlx::query_scalar(
            "insert into accounts (email) values ('c@example.com') returning id",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();

        let err = sqlx::query("update accounts set status = 'banana' where id = $1")
            .bind(account)
            .execute(&db.pool)
            .await
            .unwrap_err();
        assert_eq!(constraint_code(&err), "23514");

        let agent: uuid::Uuid = sqlx::query_scalar(
            "insert into agents (account_id, name, nl_definition) values ($1, 'a', 'b') returning id",
        )
        .bind(account)
        .fetch_one(&db.pool)
        .await
        .unwrap();

        let err = sqlx::query("update agents set status = 'retired' where id = $1")
            .bind(agent)
            .execute(&db.pool)
            .await
            .unwrap_err();
        assert_eq!(constraint_code(&err), "23514");

        let err = sqlx::query(
            "insert into agent_runs (agent_id, account_id, trigger_kind) values ($1, $2, 'telepathy')",
        )
        .bind(agent)
        .bind(account)
        .execute(&db.pool)
        .await
        .unwrap_err();
        assert_eq!(constraint_code(&err), "23514");

        let err = sqlx::query(
            "insert into feed_items (account_id, kind, title) values ($1, 'gossip', 't')",
        )
        .bind(account)
        .execute(&db.pool)
        .await
        .unwrap_err();
        assert_eq!(constraint_code(&err), "23514");
    }

    /// Criterion B7 + edge case 4 (duplicate delivery / replay).
    #[tokio::test]
    async fn agent_runs_dedupe_key_is_unique_and_nullable() {
        let db = test_db().await;
        let account: uuid::Uuid = sqlx::query_scalar(
            "insert into accounts (email) values ('d@example.com') returning id",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        let agent: uuid::Uuid = sqlx::query_scalar(
            "insert into agents (account_id, name, nl_definition) values ($1, 'a', 'b') returning id",
        )
        .bind(account)
        .fetch_one(&db.pool)
        .await
        .unwrap();

        let insert = |key: Option<&'static str>| {
            let pool = db.pool.clone();
            async move {
                sqlx::query(
                    "insert into agent_runs (agent_id, account_id, trigger_kind, dedupe_key) \
                     values ($1, $2, 'mail', $3)",
                )
                .bind(agent)
                .bind(account)
                .bind(key)
                .execute(&pool)
                .await
            }
        };

        insert(Some("history:42")).await.unwrap();
        let err = insert(Some("history:42")).await.unwrap_err();
        assert_eq!(constraint_code(&err), "23505");

        // Nulls are distinct: manual runs may all leave it unset.
        insert(None).await.unwrap();
        insert(None).await.unwrap();
    }

    /// Criterion I1 - the P2 `attachments` table, exactly as briefed. The
    /// negative half matters most: there is no bytes column, and there never
    /// will be.
    #[tokio::test]
    async fn attachments_table_matches_the_brief() {
        let db = test_db().await;
        let columns: Vec<(String, String, String)> = sqlx::query_as(
            "select column_name, data_type, is_nullable from information_schema.columns \
             where table_name = 'attachments' order by column_name",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();

        let by_name: std::collections::BTreeMap<_, _> = columns
            .iter()
            .map(|(n, t, nullable)| (n.as_str(), (t.as_str(), nullable.as_str())))
            .collect();

        assert_eq!(by_name["message_id"], ("bigint", "NO"));
        assert_eq!(by_name["att_id"], ("text", "NO"));
        assert_eq!(by_name["name"], ("text", "NO"));
        assert_eq!(by_name["mime"], ("text", "NO"));
        assert_eq!(by_name["size_bytes"], ("bigint", "NO"));
        assert_eq!(by_name["content_id"], ("text", "YES"));
        assert_eq!(by_name["inline"], ("boolean", "NO"));

        for forbidden in ["bytes", "data", "content", "body", "blob", "payload"] {
            assert!(
                !by_name.contains_key(forbidden),
                "attachment bytes are never stored, but `{forbidden}` exists"
            );
        }

        // Unique on (message_id, att_id).
        let account: uuid::Uuid =
            sqlx::query_scalar("insert into accounts (email) values ('a@x.com') returning id")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        let message: i64 = sqlx::query_scalar(
            "insert into messages (account_id, gmail_id, thread_id) \
             values ($1, 'g1', 't1') returning id",
        )
        .bind(account)
        .fetch_one(&db.pool)
        .await
        .unwrap();

        let insert = |name: &'static str| {
            let pool = db.pool.clone();
            async move {
                sqlx::query(
                    "insert into attachments (message_id, att_id, name) values ($1, 'A', $2)",
                )
                .bind(message)
                .bind(name)
                .execute(&pool)
                .await
            }
        };
        insert("first").await.unwrap();
        let err = insert("second").await.unwrap_err();
        assert_eq!(constraint_code(&err), "23505");
    }

    /// Criterion I2.
    #[tokio::test]
    async fn attachments_cascade_from_messages() {
        let db = test_db().await;
        let account: uuid::Uuid =
            sqlx::query_scalar("insert into accounts (email) values ('b@x.com') returning id")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        let message: i64 = sqlx::query_scalar(
            "insert into messages (account_id, gmail_id, thread_id) \
             values ($1, 'g1', 't1') returning id",
        )
        .bind(account)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        sqlx::query("insert into attachments (message_id, att_id) values ($1, 'A')")
            .bind(message)
            .execute(&db.pool)
            .await
            .unwrap();

        sqlx::query("delete from messages where id = $1")
            .bind(message)
            .execute(&db.pool)
            .await
            .unwrap();
        let left: i64 = sqlx::query_scalar("select count(*) from attachments")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }

    /// Criteria I3 + I4 - a second settings row is impossible, by DDL.
    #[tokio::test]
    async fn settings_is_a_singleton() {
        let db = test_db().await;
        let (singleton, default): (bool, bool) =
            sqlx::query_as("select singleton, approval_required_default from settings")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert!(singleton);
        assert!(default, "approval_required_default is true by default");

        // Same key: unique violation.
        let err = sqlx::query("insert into settings (singleton) values (true)")
            .execute(&db.pool)
            .await
            .unwrap_err();
        assert_eq!(constraint_code(&err), "23505");

        // Different key: the check constraint refuses it, so there is no way in.
        let err = sqlx::query("insert into settings (singleton) values (false)")
            .execute(&db.pool)
            .await
            .unwrap_err();
        assert_eq!(constraint_code(&err), "23514");

        let rows: i64 = sqlx::query_scalar("select count(*) from settings")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(rows, 1);
    }

    /// Criterion I5 - API.md §5: a created agent is always a draft.
    #[tokio::test]
    async fn a_created_agent_is_a_draft() {
        let db = test_db().await;
        let account: uuid::Uuid =
            sqlx::query_scalar("insert into accounts (email) values ('c@x.com') returning id")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        let status: String = sqlx::query_scalar(
            "insert into agents (account_id, name, nl_definition) \
             values ($1, 'a', 'b') returning status",
        )
        .bind(account)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(status, "draft");

        let (nullable, default): (String, Option<String>) = sqlx::query_as(
            "select is_nullable, column_default from information_schema.columns \
             where table_name = 'agents' and column_name = 'status'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(nullable, "NO");
        assert!(
            default.as_deref().unwrap_or_default().contains("'draft'"),
            "{default:?}"
        );
    }

    /// Criterion B5.
    #[tokio::test]
    async fn migrations_are_idempotent() {
        let db = test_db().await;
        // `test_db` already migrated once; do it twice more.
        migrate(&db.pool).await.unwrap();
        migrate(&db.pool).await.unwrap();

        let applied: i64 = sqlx::query_scalar("select count(*) from _sqlx_migrations")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        // Derived from the migrator, not written down. A hardcoded 1 survived
        // until P3 added a second migration, and a hardcoded 2 would survive
        // exactly as long as the third (D44's lesson, applied here).
        let expected = i64::try_from(MIGRATOR.iter().count()).unwrap();
        assert_eq!(
            applied, expected,
            "every migration must be recorded exactly once, however many there are"
        );
    }

    /// Criterion B6 + C4.
    #[tokio::test]
    async fn migrations_apply_to_two_fresh_databases() {
        let a = test_db().await;
        let b = test_db().await;
        assert_ne!(a.name, b.name);
        for db in [&a, &b] {
            let tables: i64 = sqlx::query_scalar(
                "select count(*) from information_schema.tables \
                 where table_schema = 'public' and table_name <> '_sqlx_migrations'",
            )
            .fetch_one(&db.pool)
            .await
            .unwrap();
            assert_eq!(tables, 18, "{} has the wrong table count", db.name);
        }
    }

    /// Criterion C4 - isolation between tests.
    #[tokio::test]
    async fn each_test_gets_an_isolated_database() {
        let a = test_db().await;
        let b = test_db().await;
        sqlx::query("insert into accounts (email) values ('only-in-a@example.com')")
            .execute(&a.pool)
            .await
            .unwrap();

        let seen: i64 = sqlx::query_scalar("select count(*) from accounts")
            .fetch_one(&b.pool)
            .await
            .unwrap();
        assert_eq!(seen, 0, "database {} leaked rows from {}", b.name, a.name);
    }

    /// Criterion C1 - `DATABASE_URL` short-circuits the embedded server.
    #[tokio::test]
    async fn explicit_database_url_is_used_verbatim() {
        let db = test_db().await;
        let mut config = sample();
        config.env = Env::Prod;
        config.database_url = Some(db.url.clone());
        // Deliberately unusable: if anything tried to boot the embedded server
        // through this config it would fail rather than quietly succeed.
        config.embedded.port = 1;
        config.embedded.data_dir = "/nonexistent/nade".into();

        let connected = connect(&config).await.unwrap();
        assert_eq!(connected.url, db.url);
        let one: i32 = sqlx::query_scalar("select 1")
            .fetch_one(&connected.pool)
            .await
            .unwrap();
        assert_eq!(one, 1);
    }

    /// Criterion C3 - production never silently boots an embedded database.
    #[tokio::test]
    async fn prod_without_database_url_fails() {
        let mut config = sample();
        config.env = Env::Prod;
        config.database_url = None;

        let error = connect(&config).await.unwrap_err().to_string();
        assert!(error.contains("DATABASE_URL"), "{error}");
        assert!(error.contains("dev"), "{error}");
    }

    /// Enforces backend/DECISIONS.md D1 mechanically: `sqlx::query!` and
    /// friends must never appear, so `cargo build` can never need a live
    /// database or a `.sqlx` cache. `sqlx::migrate!` is fine - it reads .sql
    /// files off disk at compile time and talks to nothing.
    #[test]
    fn no_compile_time_query_macros() {
        let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
        let mut offenders = Vec::new();
        let mut stack = vec![root];

        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                // Assembled at runtime so this very test is not an offender.
                let bang = '!';
                let banned: Vec<String> = [
                    "query",
                    "query_as",
                    "query_scalar",
                    "query_file",
                    "query_file_as",
                    "query_file_scalar",
                ]
                .iter()
                .map(|name| format!("{name}{bang}("))
                .collect();

                let text = std::fs::read_to_string(&path).unwrap();
                for (number, line) in text.lines().enumerate() {
                    if line.trim_start().starts_with("//") {
                        continue;
                    }
                    if banned.iter().any(|needle| line.contains(needle.as_str())) {
                        offenders.push(format!(
                            "{}:{}: {}",
                            path.display(),
                            number + 1,
                            line.trim()
                        ));
                    }
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "compile-time sqlx query macros are banned (DECISIONS.md D1):\n{}",
            offenders.join("\n")
        );
    }

    /// Criterion S12 - `jobs.dedupe_key` collapses a burst into one pending
    /// row, and nulls stay distinct so every P1/P2 enqueue site is untouched.
    #[tokio::test]
    async fn jobs_dedupe_key_is_unique_over_pending_rows_and_nullable() {
        let db = test_db().await;

        let insert = |key: Option<&'static str>| {
            let pool = db.pool.clone();
            async move {
                sqlx::query("insert into jobs (kind, dedupe_key) values ('gmail_incremental', $1)")
                    .bind(key)
                    .execute(&pool)
                    .await
            }
        };

        insert(Some("gmail_incremental:acct")).await.unwrap();
        let err = insert(Some("gmail_incremental:acct")).await.unwrap_err();
        assert_eq!(constraint_code(&err), "23505");

        // Nulls are distinct: every enqueue that predates P3 leaves it unset.
        insert(None).await.unwrap();
        insert(None).await.unwrap();

        // A finished row leaves the index, so the next push can enqueue again.
        sqlx::query("update jobs set done_at = now() where dedupe_key is not null")
            .execute(&db.pool)
            .await
            .unwrap();
        insert(Some("gmail_incremental:acct")).await.unwrap();
    }

    /// The index predicate must not mention `locked_by`.
    ///
    /// Excluding locked rows is tempting - it would let a push enqueue while an
    /// earlier walk is still running - but `Queue::fail`, `Queue::release` and
    /// `Queue::reap_expired_leases` all set `locked_by = null`. With
    /// `locked_by is null` in the predicate, a replacement row inserted during
    /// the run would collide the instant the original failed, was released at
    /// shutdown, or had its lease reaped: the queue would be unable to record
    /// its own failures during precisely the outage that caused them.
    ///
    /// This test is the regression. It fails loudly against that predicate and
    /// passes against the one we shipped.
    #[tokio::test]
    async fn a_running_job_can_still_fail_release_and_be_reaped_with_a_pending_twin() {
        let db = test_db().await;

        let running: i64 = sqlx::query_scalar(
            "insert into jobs (kind, dedupe_key, locked_by, lease_expires_at) \
             values ('gmail_incremental', 'k', 'worker-1', now() - interval '1 minute') \
             returning id",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();

        // A second notification arrives. Under the shipped predicate the
        // running row is still indexed, so this is suppressed rather than
        // inserted - which is what keeps the three updates below legal.
        let second = sqlx::query(
            "insert into jobs (kind, dedupe_key) values ('gmail_incremental', 'k') \
             on conflict (dedupe_key) \
                 where dedupe_key is not null and done_at is null and dead_at is null \
             do nothing",
        )
        .execute(&db.pool)
        .await
        .unwrap();
        assert_eq!(
            second.rows_affected(),
            0,
            "a pending row with this key already exists"
        );

        // Now the three writers that clear `locked_by`. Each must succeed.
        for statement in [
            "update jobs set locked_by = null, lease_expires_at = null, attempts = attempts + 1 \
               where id = $1",
            "update jobs set locked_by = 'worker-1' where id = $1",
            "update jobs set locked_by = null, lease_expires_at = null where id = $1",
        ] {
            sqlx::query(statement)
                .bind(running)
                .execute(&db.pool)
                .await
                .unwrap_or_else(|e| {
                    panic!("clearing locked_by must never violate the dedupe index: {e}")
                });
        }
    }

    /// Criterion S1/S5 - the columns P3's scheduler and sweep read.
    #[tokio::test]
    async fn sync_state_carries_the_p3_columns() {
        let db = test_db().await;
        let columns: Vec<(String, String)> = sqlx::query_as(
            "select column_name, data_type from information_schema.columns \
             where table_name = 'sync_state'",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        let by_name: std::collections::BTreeMap<_, _> = columns
            .iter()
            .map(|(n, t)| (n.as_str(), t.as_str()))
            .collect();

        for (column, ty) in [
            ("watch_renewed_at", "timestamp with time zone"),
            ("watch_topic", "text"),
            ("last_checked_at", "timestamp with time zone"),
            ("last_webhook_at", "timestamp with time zone"),
            ("reconcile_after", "bigint"),
        ] {
            assert_eq!(by_name.get(column), Some(&ty), "sync_state.{column}");
        }
    }

    fn constraint_code(error: &sqlx::Error) -> String {
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .map_or_else(
                || format!("<not a database error: {error}>"),
                std::borrow::Cow::into_owned,
            )
    }
}
