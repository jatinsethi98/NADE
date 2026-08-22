//! `GET /drafts` and `PATCH /drafts/{id}`.
//!
//! There is deliberately **no** `GET /drafts/{id}` (`API.md` §11): the list
//! carries the whole draft and `PATCH` returns it, so a third way to read one
//! would be a third thing to keep consistent.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    // The tool's own caps and address rule, imported rather than restated, so
    // an edit cannot produce a draft an agent could not have written even after
    // one of the numbers moves.
    agents::{
        fence,
        tools::draft_reply::{valid_address, MAX_BODY_BYTES, MAX_RECIPIENTS, MAX_SUBJECT_BYTES},
    },
    api::{auth::Auth, cursor, cursor::Keyset, mail::wire_ts},
    error::{ApiError, ApiJson, ApiResult},
    state::AppState,
};

const PAGE_ROWS: usize = 50;
/// The same number, in the type the `limit` bind wants.
const PAGE: i64 = PAGE_ROWS as i64;

#[derive(Debug, Deserialize)]
pub struct DraftsQuery {
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Draft {
    pub id: Uuid,
    pub run_id: Option<Uuid>,
    pub thread_id: Option<String>,
    pub to: Vec<String>,
    pub subject: String,
    pub body_text: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct DraftsResponse {
    pub drafts: Vec<Draft>,
    pub next_cursor: Option<String>,
}

/// Any subset. An empty object is `400` (`API.md` §3) — a `PATCH` that changes
/// nothing is a caller bug, and answering `200` would hide it.
#[derive(Debug, Deserialize)]
pub struct DraftPatch {
    pub to: Option<Vec<String>>,
    pub subject: Option<String>,
    pub body_text: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct DraftRow {
    id: Uuid,
    run_id: Option<Uuid>,
    thread_id: Option<String>,
    to_json: Value,
    subject: Option<String>,
    body_text: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl DraftRow {
    fn into_view(self) -> Draft {
        Draft {
            id: self.id,
            run_id: self.run_id,
            thread_id: self.thread_id,
            to: crate::json::str_array(Some(&self.to_json))
                .into_iter()
                .map(str::to_owned)
                .collect(),
            // `subject` is nullable in the column and never null on the wire.
            subject: self.subject.unwrap_or_default(),
            body_text: self.body_text,
            created_at: wire_ts(self.created_at),
            updated_at: wire_ts(self.updated_at),
        }
    }
}

/// `GET /v1/drafts?cursor`.
pub async fn list(
    State(state): State<AppState>,
    auth: Auth,
    Query(query): Query<DraftsQuery>,
) -> ApiResult<Json<DraftsResponse>> {
    let keyset = query.cursor.as_deref().map(cursor::decode).transpose()?;

    let Some(account) = auth.account else {
        return Ok(Json(DraftsResponse {
            drafts: Vec::new(),
            next_cursor: None,
        }));
    };

    let mut rows: Vec<DraftRow> = sqlx::query_as(
        "select id, run_id, thread_id, to_json, subject, body_text, created_at, updated_at \
           from drafts \
          where account_id = $1 \
            and ($2::timestamptz is null \
                 or (updated_at, id) < ($2::timestamptz, $3::uuid)) \
          order by updated_at desc, id desc \
          limit $4",
    )
    .bind(account.id)
    .bind(keyset.as_ref().map(|k| k.ts))
    .bind(keyset.as_ref().map(Keyset::uuid).transpose()?)
    .bind(PAGE + 1)
    .fetch_all(&state.pool)
    .await?;

    let next_cursor = cursor::take_page(&mut rows, PAGE_ROWS, |row| {
        (row.updated_at, row.id.to_string())
    });

    Ok(Json(DraftsResponse {
        drafts: rows.into_iter().map(DraftRow::into_view).collect(),
        next_cursor,
    }))
}

/// `PATCH /v1/drafts/{id}` — the only edit path v1 has.
pub async fn patch(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<DraftPatch>,
) -> ApiResult<Json<Draft>> {
    let account = auth.account.ok_or_else(ApiError::not_found)?;

    if body.to.is_none() && body.subject.is_none() && body.body_text.is_none() {
        return Err(ApiError::bad_request(
            "Send at least one of `to`, `subject` or `body_text`.",
        ));
    }

    // Validated before the write, and with the same rules the tool applies -
    // an edit must not be able to produce a draft an agent could not have.
    let recipients = match body.to {
        None => None,
        Some(list) => {
            if list.is_empty() {
                return Err(ApiError::bad_request(
                    "`to` must contain at least one address.",
                ));
            }
            if list.len() > MAX_RECIPIENTS {
                return Err(ApiError::bad_request(format!(
                    "`to` may hold at most {MAX_RECIPIENTS} addresses."
                )));
            }
            let mut cleaned = Vec::with_capacity(list.len());
            for address in list {
                let address = fence::strip_control_characters(address.trim());
                // The tool's own rule, not a second one: an edit must not be
                // able to produce a draft an agent could not have written.
                if !valid_address(&address) {
                    return Err(ApiError::bad_request(format!(
                        "{address:?} is not an email address."
                    )));
                }
                cleaned.push(address);
            }
            Some(cleaned)
        }
    };

    let subject = body
        .subject
        .map(|value| fence::stored(&value, MAX_SUBJECT_BYTES));
    let text = body
        .body_text
        .map(|value| fence::stored(&value, MAX_BODY_BYTES));

    // `coalesce` per column, so an absent field is left exactly as it was
    // rather than being read, merged and written back - two concurrent patches
    // of different fields must not undo each other (the same defect P3's iOS
    // review found in the agent PATCH path).
    let row: Option<DraftRow> = sqlx::query_as(
        "update drafts set \
            to_json = coalesce($3::jsonb, to_json), \
            subject = coalesce($4::text, subject), \
            body_text = coalesce($5::text, body_text), \
            updated_at = now() \
          where id = $1 and account_id = $2 \
         returning id, run_id, thread_id, to_json, subject, body_text, created_at, updated_at",
    )
    .bind(id)
    .bind(account.id)
    .bind(recipients.map(Value::from))
    .bind(subject)
    .bind(text)
    .fetch_optional(&state.pool)
    .await?;

    row.map(|row| Json(row.into_view()))
        .ok_or_else(ApiError::not_found)
}

#[cfg(test)]
mod tests;
