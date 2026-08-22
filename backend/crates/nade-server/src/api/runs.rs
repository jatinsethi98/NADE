//! `GET /runs` and `GET /runs/{id}` — the Run log, and one run's journal.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    api::{auth::Auth, cursor, cursor::Keyset, mail::wire_ts},
    error::{ApiError, ApiResult},
    state::AppState,
};

/// `API.md` §0 fixes `GET /runs` at 50 per page.
const PAGE_ROWS: usize = 50;
/// The same number, in the type the `limit` bind wants.
const PAGE: i64 = PAGE_ROWS as i64;

#[derive(Debug, Deserialize)]
pub struct RunsQuery {
    pub agent_id: Option<Uuid>,
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RunSummary {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub agent_name: String,
    pub trigger_kind: String,
    pub status: String,
    pub summary: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct RunsResponse {
    pub runs: Vec<RunSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct JournalEntry {
    pub seq: i32,
    pub kind: String,
    pub payload: Value,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct RunDetail {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub agent_name: String,
    pub status: String,
    pub trigger_kind: String,
    pub trigger_ref: Option<String>,
    pub error: Option<String>,
    pub journal: Vec<JournalEntry>,
}

/// The detail read: the run plus its agent's name.
#[derive(Debug, sqlx::FromRow)]
struct RunHeadRow {
    id: Uuid,
    agent_id: Uuid,
    agent_name: String,
    status: String,
    trigger_kind: String,
    trigger_ref: Option<String>,
    error: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct RunRow {
    id: Uuid,
    agent_id: Uuid,
    agent_name: String,
    trigger_kind: String,
    status: String,
    summary: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl RunRow {
    fn into_view(self) -> RunSummary {
        RunSummary {
            id: self.id,
            agent_id: self.agent_id,
            agent_name: self.agent_name,
            trigger_kind: self.trigger_kind,
            status: self.status,
            summary: self.summary,
            created_at: wire_ts(self.created_at),
            updated_at: wire_ts(self.updated_at),
        }
    }
}

/// `GET /v1/runs?agent_id&cursor` — paginated, newest first.
pub async fn list(
    State(state): State<AppState>,
    auth: Auth,
    Query(query): Query<RunsQuery>,
) -> ApiResult<Json<RunsResponse>> {
    // An unknown or corrupt cursor is a 400, never a silent reset to page one
    // (`API.md` §0). Decoded before the account check so a bad cursor answers
    // the same way whether or not a mailbox is connected.
    let keyset = query.cursor.as_deref().map(cursor::decode).transpose()?;

    let Some(account) = auth.account else {
        // `API.md` §0: an empty collection is an empty array and a null cursor,
        // never a 404.
        return Ok(Json(RunsResponse {
            runs: Vec::new(),
            next_cursor: None,
        }));
    };

    // One more than the page, so the presence of the extra row answers "is
    // there a next page" without a second count query.
    let mut rows: Vec<RunRow> = sqlx::query_as(
        "select r.id, r.agent_id, a.name as agent_name, r.trigger_kind, r.status, \
                r.summary, r.created_at, r.updated_at \
           from agent_runs r join agents a on a.id = r.agent_id \
          where r.account_id = $1 \
            and ($2::uuid is null or r.agent_id = $2) \
            and ($3::timestamptz is null \
                 or (r.created_at, r.id) < ($3::timestamptz, $4::uuid)) \
          order by r.created_at desc, r.id desc \
          limit $5",
    )
    .bind(account.id)
    .bind(query.agent_id)
    .bind(keyset.as_ref().map(|k| k.ts))
    .bind(keyset.as_ref().map(Keyset::uuid).transpose()?)
    .bind(PAGE + 1)
    .fetch_all(&state.pool)
    .await?;

    let next_cursor = cursor::take_page(&mut rows, PAGE_ROWS, |row| {
        (row.created_at, row.id.to_string())
    });

    Ok(Json(RunsResponse {
        runs: rows.into_iter().map(RunRow::into_view).collect(),
        next_cursor,
    }))
}

/// `GET /v1/runs/{id}` — the run, and its journal **verbatim**.
///
/// Verbatim is the contract (`API.md` §6.1): `run_journal` has exactly one
/// author, the engine, and this endpoint does not translate, reorder or
/// summarise what it wrote. A host that rewrote a payload on the way out would
/// be a second vocabulary layered on the first.
pub async fn detail(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<RunDetail>> {
    let account = auth.account.ok_or_else(ApiError::not_found)?;

    // Account-scoped in the predicate, so another account's run is a 404 and
    // never a 403 - the caller learns nothing about what exists.
    let row: Option<RunHeadRow> = sqlx::query_as(
        "select r.id, r.agent_id, a.name as agent_name, r.status, r.trigger_kind, \
                r.trigger_ref, r.error \
           from agent_runs r join agents a on a.id = r.agent_id \
          where r.id = $1 and r.account_id = $2",
    )
    .bind(id)
    .bind(account.id)
    .fetch_optional(&state.pool)
    .await?;

    let Some(head) = row else {
        return Err(ApiError::not_found());
    };

    let entries: Vec<(i32, String, Value, DateTime<Utc>)> = sqlx::query_as(
        "select seq, kind, payload, created_at from run_journal \
          where run_id = $1 order by seq asc",
    )
    .bind(head.id)
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(RunDetail {
        id: head.id,
        agent_id: head.agent_id,
        agent_name: head.agent_name,
        status: head.status,
        trigger_kind: head.trigger_kind,
        trigger_ref: head.trigger_ref,
        error: head.error,
        journal: entries
            .into_iter()
            .map(|(seq, kind, payload, created_at)| JournalEntry {
                seq,
                kind,
                payload,
                created_at: wire_ts(created_at),
            })
            .collect(),
    }))
}

#[cfg(test)]
mod tests;
