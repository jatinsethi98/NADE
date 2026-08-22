//! `GET /notes` and `GET /notes/{id}`.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    api::{auth::Auth, cursor, cursor::Keyset, mail::wire_ts},
    error::{ApiError, ApiResult},
    state::AppState,
};

const PAGE_ROWS: usize = 50;
/// The same number, in the type the `limit` bind wants.
const PAGE: i64 = PAGE_ROWS as i64;

/// Neutralise `ilike`'s wildcards so a search means what it says.
///
/// `%` and `_` are pattern syntax; a user searching for a literal `%` must not
/// match every row. `\` goes first, because escaping it after the others would
/// escape the escapes.
fn escape_like(query: &str) -> String {
    query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[derive(Debug, Deserialize)]
pub struct NotesQuery {
    pub q: Option<String>,
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct NoteSummary {
    pub id: Uuid,
    pub run_id: Option<Uuid>,
    pub thread_id: Option<String>,
    pub title: String,
    pub agent_name: Option<String>,
    pub unread: bool,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct NotesResponse {
    pub notes: Vec<NoteSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct NoteDetail {
    pub id: Uuid,
    pub title: String,
    pub body_md: String,
    pub run_id: Option<Uuid>,
    pub thread_id: Option<String>,
    pub agent_name: Option<String>,
    pub unread: bool,
    pub updated_at: String,
}

/// The detail read, which is also the update's `returning` list.
#[derive(Debug, sqlx::FromRow)]
struct NoteDetailRow {
    id: Uuid,
    title: String,
    body_md: String,
    run_id: Option<Uuid>,
    thread_id: Option<String>,
    agent_name: Option<String>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, sqlx::FromRow)]
struct NoteRow {
    id: Uuid,
    run_id: Option<Uuid>,
    thread_id: Option<String>,
    title: String,
    agent_name: Option<String>,
    unread: bool,
    updated_at: DateTime<Utc>,
}

/// `GET /v1/notes?q&cursor`.
///
/// `q` is a plain case-insensitive substring scan over `title` and `body_md`,
/// deliberately (`API.md` §3): notes are our own rows so Gmail cannot search
/// them, `docs/SEARCH.md` forbids standing up a second index, and the dev caps
/// bound this table to tens of rows. Indexing hundreds costs more than scanning
/// them.
pub async fn list(
    State(state): State<AppState>,
    auth: Auth,
    Query(query): Query<NotesQuery>,
) -> ApiResult<Json<NotesResponse>> {
    let keyset = query.cursor.as_deref().map(cursor::decode).transpose()?;

    let Some(account) = auth.account else {
        return Ok(Json(NotesResponse {
            notes: Vec::new(),
            next_cursor: None,
        }));
    };

    // `ilike`, with `q`'s own wildcards escaped. Concatenating the pattern in
    // SQL does *not* neutralise them: a search for "%" would otherwise become
    // `ilike '%%%'` and match every note in the mailbox. `\` is escaped first,
    // or escaping the others would double-escape it.
    let mut rows: Vec<NoteRow> = sqlx::query_as(
        "select n.id, n.run_id, n.thread_id, n.title, a.name as agent_name, n.unread, \
                n.updated_at \
           from notes n \
           left join agent_runs r on r.id = n.run_id \
           left join agents a on a.id = r.agent_id \
          where n.account_id = $1 \
            and ($2::text is null \
                 or n.title ilike '%' || $2 || '%' escape '\\' \
                 or n.body_md ilike '%' || $2 || '%' escape '\\') \
            and ($3::timestamptz is null \
                 or (n.updated_at, n.id) < ($3::timestamptz, $4::uuid)) \
          order by n.updated_at desc, n.id desc \
          limit $5",
    )
    .bind(account.id)
    .bind(
        query
            .q
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(escape_like),
    )
    .bind(keyset.as_ref().map(|k| k.ts))
    .bind(keyset.as_ref().map(Keyset::uuid).transpose()?)
    .bind(PAGE + 1)
    .fetch_all(&state.pool)
    .await?;

    let next_cursor = cursor::take_page(&mut rows, PAGE_ROWS, |row| {
        (row.updated_at, row.id.to_string())
    });

    Ok(Json(NotesResponse {
        notes: rows
            .into_iter()
            .map(|row| NoteSummary {
                id: row.id,
                run_id: row.run_id,
                thread_id: row.thread_id,
                title: row.title,
                agent_name: row.agent_name,
                unread: row.unread,
                updated_at: wire_ts(row.updated_at),
            })
            .collect(),
        next_cursor,
    }))
}

/// `GET /v1/notes/{id}` — and reading it marks it read.
///
/// The response therefore always reports `unread: false`: it describes the state
/// *after* the read, not the one the list showed (`API.md` §3). Reading twice is
/// not an error.
pub async fn detail(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<NoteDetail>> {
    let account = auth.account.ok_or_else(ApiError::not_found)?;

    // One statement: clear the marker, return the row, and pick up the agent's
    // name on the way out. Two would leave a window where a concurrent read saw
    // `unread: true` for a note already being read; three - which is what the
    // separate `agent_name` lookup made it - added a round trip to every note
    // opened, for a join `list` already spells out seventy lines above.
    //
    // `updated_at` must NOT move: opening a note is not an edit, and the list
    // is ordered by it.
    //
    // `agent_name` is null for a note with no run behind it (`API.md` §3),
    // which is what the two `left join`s deliver.
    let row: Option<NoteDetailRow> = sqlx::query_as(
        "with opened as ( \
             update notes set unread = false \
              where id = $1 and account_id = $2 \
             returning id, title, body_md, run_id, thread_id, updated_at) \
         select o.id, o.title, o.body_md, o.run_id, o.thread_id, a.name as agent_name, \
                o.updated_at \
           from opened o \
           left join agent_runs r on r.id = o.run_id \
           left join agents a on a.id = r.agent_id",
    )
    .bind(id)
    .bind(account.id)
    .fetch_optional(&state.pool)
    .await?;

    let Some(note) = row else {
        return Err(ApiError::not_found());
    };

    Ok(Json(NoteDetail {
        id: note.id,
        title: note.title,
        body_md: note.body_md,
        run_id: note.run_id,
        thread_id: note.thread_id,
        agent_name: note.agent_name,
        // The state *after* the read, which is what `API.md` §3 specifies.
        unread: false,
        updated_at: wire_ts(note.updated_at),
    }))
}

#[cfg(test)]
mod tests;
