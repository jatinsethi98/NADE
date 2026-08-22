//! `run_journal` as an SDK [`Journal`].
//!
//! # What this table is
//!
//! The append-only record of everything one agent run did, written by exactly
//! one author — the engine — and served verbatim by `GET /runs/{id}`
//! (`API.md` §6.1). The server never appends a row of its own; a human's
//! approval reaches the journal only as the `approval_resolved` that
//! `Engine::resume` writes.
//!
//! # The two promises this file has to keep
//!
//! **Durable before `Ok`.** The SDK's `Journal::append` doc is unusually blunt
//! about it: "Returning `Ok` is a promise that the entry would survive the
//! process being killed at the next instruction... An implementation that
//! returns before the write is durable has not made the guarantee weaker — it
//! has removed it, silently, from every run." The engine appends
//! `step_started`, waits for that `Ok`, and only then performs a side effect it
//! cannot take back.
//!
//! **Byte-faithful.** `created_at` is the *engine's* stamp, truncated to whole
//! seconds by `Entry::new`/`Engine::now` and floored so it can never go
//! backwards. It is not the database's clock, and the column's `default now()`
//! is a live trap: leaving the column out of the insert silently substitutes
//! PostgreSQL's clock and breaks both the contract and, on replay, the engine's
//! own drift checks.
//!
//! # Durability is a property of the cluster, not of this code
//!
//! `embedded.rs` deliberately starts the dev cluster with `fsync=off`,
//! `synchronous_commit=off` and `full_page_writes=off` — "this cluster holds
//! nothing but dev scratch and per-test databases, so trade crash durability
//! for speed". That is the right call and must not be "fixed": those settings
//! lose committed data on an **OS crash or power loss**, not on the client
//! process being killed, so every kill-mid-run test here still proves exactly
//! what it claims. What they do mean is that `append` cannot rely on the
//! cluster's default, which is why it sets `synchronous_commit` for its own
//! transaction. In production the deploy checklist owns the rest.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use nade_agent_sdk::{
    Entry, EntryKind, Error as SdkError, Journal, Result as SdkResult, RunId, Seq,
};
use serde_json::Value;
use sqlx::{PgPool, Row};

/// PostgreSQL's `unique_violation`. The only error `append` translates rather
/// than wraps: the engine reads a `SeqConflict` as "another writer got there
/// first" and anything else as "storage is broken", and they route differently.
const UNIQUE_VIOLATION: &str = "23505";

/// The SDK [`Journal`] over `run_journal`.
#[derive(Debug, Clone)]
pub struct PgJournal {
    pool: PgPool,
}

impl PgJournal {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// `Seq` is a `u32`; the column is `integer`. `sqlx` has no
/// `Encode<Postgres>` for `u32`, so the cast is explicit — and checked, because
/// a silent wrap would put a *negative* seq in the primary key and corrupt the
/// ordering the whole protocol depends on.
fn seq_to_db(run: RunId, seq: Seq) -> SdkResult<i32> {
    i32::try_from(seq).map_err(|_| SdkError::Journal {
        message: format!("run {run} reached seq {seq}, which exceeds this column's range"),
        source: None,
    })
}

fn seq_from_db(run: RunId, raw: i32) -> SdkResult<Seq> {
    Seq::try_from(raw).map_err(|_| SdkError::Journal {
        message: format!("run {run} has a negative seq ({raw}) in run_journal"),
        source: None,
    })
}

fn journal_error(message: impl Into<String>, source: sqlx::Error) -> SdkError {
    SdkError::journal_source(message, source)
}

#[async_trait]
impl Journal for PgJournal {
    async fn append(&self, run: RunId, entry: Entry) -> SdkResult<Seq> {
        let seq = entry.seq;
        let db_seq = seq_to_db(run, seq)?;

        // A transaction, for one statement, on purpose: `set local` is scoped
        // to a transaction and is the only way to raise `synchronous_commit`
        // for this write regardless of what the cluster defaults to. Without
        // it, `Ok` on a cluster configured for speed is not the promise the
        // SDK's contract says it is.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| journal_error(format!("opening a transaction for run {run}"), e))?;

        sqlx::query("set local synchronous_commit = on")
            .execute(&mut *tx)
            .await
            .map_err(|e| journal_error(format!("hardening the commit for run {run}"), e))?;

        let result = sqlx::query(
            "insert into run_journal (run_id, seq, kind, payload, created_at) \
             values ($1, $2, $3, $4, $5)",
        )
        .bind(run.as_uuid())
        .bind(db_seq)
        .bind(entry.kind.as_str())
        .bind(&entry.payload)
        // Bound explicitly. The column carries `default now()`, so omitting it
        // would quietly substitute the database's clock for the engine's and
        // break the byte-faithfulness `API.md` §6.1 promises.
        .bind(entry.created_at)
        .execute(&mut *tx)
        .await;

        match result {
            Ok(_) => {
                tx.commit()
                    .await
                    .map_err(|e| journal_error(format!("committing seq {seq} for run {run}"), e))?;
                Ok(seq)
            }
            Err(err) => {
                // The rollback is implicit when `tx` drops, but being explicit
                // keeps the failure path readable.
                drop(tx);
                if err
                    .as_database_error()
                    .and_then(sqlx::error::DatabaseError::code)
                    .as_deref()
                    == Some(UNIQUE_VIOLATION)
                {
                    return Err(SdkError::SeqConflict { run, seq });
                }
                Err(journal_error(
                    format!("appending seq {seq} to run {run}"),
                    err,
                ))
            }
        }
    }

    async fn load(&self, run: RunId) -> SdkResult<Vec<Entry>> {
        let rows = sqlx::query(
            "select seq, kind, payload, created_at from run_journal \
              where run_id = $1 order by seq asc",
        )
        .bind(run.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| journal_error(format!("loading the journal for run {run}"), e))?;

        rows.into_iter()
            .map(|row| {
                let raw_seq: i32 = row
                    .try_get("seq")
                    .map_err(|e| journal_error("reading seq", e))?;
                let kind: String = row
                    .try_get("kind")
                    .map_err(|e| journal_error("reading kind", e))?;
                let payload: Value = row
                    .try_get("payload")
                    .map_err(|e| journal_error("reading payload", e))?;
                let created_at: DateTime<Utc> = row
                    .try_get("created_at")
                    .map_err(|e| journal_error("reading created_at", e))?;
                Ok(Entry {
                    seq: seq_from_db(run, raw_seq)?,
                    // `from_tag` round-trips an unrecognised kind through
                    // `EntryKind::Other` rather than failing to parse, so a
                    // journal written by a newer build still loads - and the
                    // engine's own replay validator is what refuses it, with a
                    // typed error, rather than a decode panic here.
                    kind: EntryKind::from_tag(&kind),
                    payload,
                    created_at,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
