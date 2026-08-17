//! The read endpoints: `/me`, `/mailboxes`, `/mailboxes/{id}/threads`,
//! `/threads/{id}`, `/search`, and the attachment proxy.
//!
//! `docs/API.md` §1 and §2 are the specification, down to the nullability of
//! every field. Two rules from it that shape the code here:
//!
//! * **`from_name` is `""`, never null.** The server does not invent a display
//!   name from the local part; the UI decides what to fall back to.
//! * **`body_html` is null exactly when there is no `text/html` part.** It is
//!   not a rendering of the plain text - "View original" keys off it.

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::cursor;
use crate::{
    error::{ApiError, ApiResult},
    gmail::client::GmailError,
    state::AppState,
};

/// `API.md` §0: attachment download cap.
pub const MAX_ATTACHMENT_BYTES: i64 = 25 * 1024 * 1024;

/// `API.md` §0: `q` is capped at 512 characters.
const MAX_QUERY_CHARS: usize = 512;

/// `API.md` §2, table order and display names. **Only** these system labels are
/// exposed; the rest (`UNREAD`, `IMPORTANT`, `CHAT`, `YELLOW_STAR`, `SPAM`,
/// `TRASH`, `DRAFT`) are flags or places v1 does not sync.
pub const SYSTEM_MAILBOXES: [(&str, &str); 8] = [
    ("INBOX", "Inbox"),
    ("CATEGORY_PERSONAL", "Primary"),
    ("CATEGORY_UPDATES", "Updates"),
    ("CATEGORY_SOCIAL", "Social"),
    ("CATEGORY_PROMOTIONS", "Promotions"),
    ("CATEGORY_FORUMS", "Forums"),
    ("STARRED", "Starred"),
    ("SENT", "Sent"),
];

/// Gmail's own internal views. Hidden from the mailbox list.
const HIDDEN_PREFIX: &str = "[Gmail]";

// ------------------------------------------------------------- wire types --

#[derive(Debug, Serialize)]
pub struct MeResponse {
    pub email: String,
    /// `ok` | `needs_reauth`.
    pub status: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Mailbox {
    pub id: String,
    pub name: String,
    /// `system` | `user`.
    pub kind: String,
    pub unread: i64,
    pub total: i64,
}

#[derive(Debug, Serialize)]
pub struct MailboxesResponse {
    pub mailboxes: Vec<Mailbox>,
}

#[derive(Debug, Serialize)]
pub struct ThreadSummary {
    pub id: String,
    pub subject: String,
    pub snippet: String,
    pub from_name: String,
    pub from_email: String,
    pub ts: String,
    pub unread: bool,
    pub msg_count: i32,
    pub agent_note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ThreadsResponse {
    pub threads: Vec<ThreadSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AttachmentView {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: i64,
    pub inline: bool,
}

#[derive(Debug, Serialize)]
pub struct MessageView {
    /// `API.md` §2: **there is no `id` field on a message.** `gmail_id` is the
    /// identity; the database's bigint primary key never leaves the server.
    pub gmail_id: String,
    pub from_name: String,
    pub from_email: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub ts: String,
    /// Never null; may be `""` for a genuinely empty body.
    pub body_text: String,
    /// Null when the message has **no** `text/html` part.
    pub body_html: Option<String>,
    pub label_ids: Vec<String>,
    pub attachments: Vec<AttachmentView>,
}

#[derive(Debug, Serialize)]
pub struct AgentCard {
    pub run_id: String,
    pub agent_name: String,
    pub status: String,
    pub summary: String,
    pub feed_item_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ThreadDetail {
    pub id: String,
    pub subject: String,
    /// The label the thread is filed under, for the footer.
    pub mailbox_name: String,
    pub account_email: String,
    /// Oldest first.
    pub messages: Vec<MessageView>,
    /// Newest first.
    pub agent_cards: Vec<AgentCard>,
}

// ---------------------------------------------------------------- queries --

#[derive(Debug, Deserialize)]
pub struct PageQuery {
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
    pub cursor: Option<String>,
}

// --------------------------------------------------------------- handlers --

/// `GET /v1/me`.
///
/// # Errors
/// Returns an error only if the database is unreachable.
pub async fn me(State(state): State<AppState>) -> ApiResult<Json<MeResponse>> {
    // JUDGEMENT: with no account connected yet, `/me` answers rather than
    // 404s. The client's whole job with a `needs_reauth` status is to show the
    // "sign in" row in Settings, which is exactly the right screen for "not
    // connected yet". Keeping `/me` total also means the iOS lane never has to
    // special-case a missing body. Reported to the contract lane.
    let account = state.account().await?;
    Ok(Json(match account {
        Some(account) => MeResponse {
            email: account.email,
            status: account.status,
        },
        None => MeResponse {
            email: String::new(),
            status: "needs_reauth".to_owned(),
        },
    }))
}

/// `GET /v1/mailboxes`. Not paginated - a bounded collection returns a plain
/// array with no cursor field at all (`API.md` §0).
///
/// # Errors
/// Returns an error only if the database is unreachable.
pub async fn mailboxes(State(state): State<AppState>) -> ApiResult<Json<MailboxesResponse>> {
    let Some(account) = state.account().await? else {
        return Ok(Json(MailboxesResponse {
            mailboxes: Vec::new(),
        }));
    };
    Ok(Json(MailboxesResponse {
        mailboxes: visible_mailboxes(&state, account.id).await?,
    }))
}

/// `GET /v1/mailboxes/{id}/threads?cursor&limit`.
///
/// # Errors
/// `400 bad_request` for an unknown cursor; otherwise only on a database
/// failure.
pub async fn threads(
    State(state): State<AppState>,
    Path(mailbox_id): Path<String>,
    Query(page): Query<PageQuery>,
) -> ApiResult<Json<ThreadsResponse>> {
    let limit = cursor::clamp_limit(page.limit, 50, 100);
    let keyset = page.cursor.as_deref().map(cursor::decode).transpose()?;

    let Some(account) = state.account().await? else {
        return Ok(Json(empty_page()));
    };

    // Ask for one more than we need: the presence of that extra row is what
    // tells us whether there is a next page, without a second count query.
    let rows: Vec<ThreadRow> = sqlx::query_as(
        "select t.thread_id, t.subject, t.snippet, t.from_name, t.from_email, \
                t.last_ts, t.unread, t.msg_count \
           from thread_labels tl \
           join threads t \
             on t.account_id = tl.account_id and t.thread_id = tl.thread_id \
          where tl.account_id = $1 \
            and tl.label_id = $2 \
            and ($3::timestamptz is null \
                 or (tl.last_ts, tl.thread_id) < ($3::timestamptz, $4::text)) \
          order by tl.last_ts desc, tl.thread_id desc \
          limit $5",
    )
    .bind(account.id)
    .bind(&mailbox_id)
    .bind(keyset.as_ref().map(|k| k.ts))
    .bind(keyset.as_ref().map(|k| k.id.clone()))
    .bind(limit + 1)
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(page_of(&state, account.id, rows, limit).await?))
}

/// `GET /v1/search?q&cursor`. Same body shape as the thread list.
///
/// # Errors
/// `400 bad_request` for an empty `q` or an unknown cursor.
pub async fn search(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> ApiResult<Json<ThreadsResponse>> {
    // EDGE (empty input): an empty or whitespace-only `q` is a 400, not "every
    // thread you own".
    let text = query.q.unwrap_or_default();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request("Type something to search for."));
    }
    if trimmed.chars().count() > MAX_QUERY_CHARS {
        return Err(ApiError::bad_request(
            "That search is too long. Try something shorter.",
        ));
    }

    let keyset = query.cursor.as_deref().map(cursor::decode).transpose()?;
    let Some(account) = state.account().await? else {
        return Ok(Json(empty_page()));
    };

    let rows: Vec<ThreadRow> = sqlx::query_as(
        "select t.thread_id, t.subject, t.snippet, t.from_name, t.from_email, \
                t.last_ts, t.unread, t.msg_count \
           from threads t \
          where t.account_id = $1 \
            and exists ( \
                select 1 from messages m \
                 where m.account_id = t.account_id \
                   and m.thread_id = t.thread_id \
                   and m.deleted_at is null \
                   and m.fts @@ websearch_to_tsquery('simple', $2)) \
            and ($3::timestamptz is null \
                 or (t.last_ts, t.thread_id) < ($3::timestamptz, $4::text)) \
          order by t.last_ts desc, t.thread_id desc \
          limit $5",
    )
    .bind(account.id)
    .bind(trimmed)
    .bind(keyset.as_ref().map(|k| k.ts))
    .bind(keyset.as_ref().map(|k| k.id.clone()))
    .bind(51i64)
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(page_of(&state, account.id, rows, 50).await?))
}

/// `GET /v1/threads/{id}`.
///
/// # Errors
/// `404 not_found` when the thread is not this account's.
pub async fn thread(
    State(state): State<AppState>,
    Path(thread_id): Path<String>,
) -> ApiResult<Json<ThreadDetail>> {
    let account = state.account().await?.ok_or_else(ApiError::not_found)?;

    let head: Option<(String,)> =
        sqlx::query_as("select subject from threads where account_id = $1 and thread_id = $2")
            .bind(account.id)
            .bind(&thread_id)
            .fetch_optional(&state.pool)
            .await?;
    let Some((subject,)) = head else {
        return Err(ApiError::not_found());
    };

    // Oldest first (`API.md` §2). `gmail_id` breaks ties so two messages with
    // the same second do not swap order between requests.
    let rows: Vec<MessageRow> = sqlx::query_as(
        "select id, gmail_id, from_name, from_email, to_json, cc_json, internal_ts, \
                body_text, body_html, label_ids \
           from messages \
          where account_id = $1 and thread_id = $2 and deleted_at is null \
          order by internal_ts asc nulls first, gmail_id asc",
    )
    .bind(account.id)
    .bind(&thread_id)
    .fetch_all(&state.pool)
    .await?;

    let ids: Vec<i64> = rows.iter().map(|row| row.id).collect();
    let attachments: Vec<AttachmentRow> = sqlx::query_as(
        "select message_id, att_id, name, mime, size_bytes, inline \
           from attachments where message_id = any($1) order by id",
    )
    .bind(&ids)
    .fetch_all(&state.pool)
    .await?;

    let messages = rows
        .into_iter()
        .map(|row| {
            let mine = attachments
                .iter()
                .filter(|attachment| attachment.message_id == row.id)
                .map(|attachment| AttachmentView {
                    id: attachment.att_id.clone(),
                    name: attachment.name.clone(),
                    mime: attachment.mime.clone(),
                    size: attachment.size_bytes,
                    inline: attachment.inline,
                })
                .collect();
            row.into_view(mine)
        })
        .collect();

    Ok(Json(ThreadDetail {
        id: thread_id.clone(),
        subject,
        mailbox_name: mailbox_name_for(&state, account.id, &thread_id).await?,
        account_email: account.email,
        messages,
        // P4/P5 fill these in; the field exists now so the shape never changes.
        agent_cards: Vec::new(),
    }))
}

/// `GET /v1/messages/{gmail_id}/attachments/{att_id}`.
///
/// Streams from Gmail on demand. The bytes are never stored and never cached:
/// `attachments` holds metadata only.
///
/// # Errors
/// `404` when we or Gmail no longer have it, `413` over 25 MB, `502` when Gmail
/// fails after retries.
pub async fn attachment(
    State(state): State<AppState>,
    Path((gmail_id, att_id)): Path<(String, String)>,
) -> ApiResult<Response> {
    let account = state.account().await?.ok_or_else(ApiError::not_found)?;

    let stored: Option<StoredAttachment> = sqlx::query_as(
        "select a.att_id, a.name, a.mime, a.size_bytes, a.content_id \
           from attachments a \
           join messages m on m.id = a.message_id \
          where m.account_id = $1 and m.gmail_id = $2 and a.att_id = $3",
    )
    .bind(account.id)
    .bind(&gmail_id)
    .bind(&att_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some(stored) = stored else {
        return Err(ApiError::not_found());
    };

    // Refuse before spending Gmail quota on something we would then throw away.
    if stored.size_bytes > MAX_ATTACHMENT_BYTES {
        return Err(ApiError::payload_too_large());
    }

    let bytes = fetch_attachment_bytes(&state, account.id, &gmail_id, &stored).await?;
    if i64::try_from(bytes.len()).unwrap_or(i64::MAX) > MAX_ATTACHMENT_BYTES {
        return Err(ApiError::payload_too_large());
    }

    let mut response = (StatusCode::OK, bytes).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&stored.mime)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&content_disposition(&stored.name))
            .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
    );
    // Never cached: the bytes are the user's mail, and we are a proxy.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, private"),
    );
    Ok(response)
}

// ---------------------------------------------------------------- helpers --

fn empty_page() -> ThreadsResponse {
    // `API.md` §0: an empty collection is an empty array **and**
    // `next_cursor: null` - never a 404.
    ThreadsResponse {
        threads: Vec::new(),
        next_cursor: None,
    }
}

/// Turn `limit + 1` rows into a page and its cursor.
async fn page_of(
    state: &AppState,
    account_id: Uuid,
    mut rows: Vec<ThreadRow>,
    limit: i64,
) -> ApiResult<ThreadsResponse> {
    let limit = usize::try_from(limit).unwrap_or(50);
    let has_more = rows.len() > limit;
    rows.truncate(limit);

    let notes = agent_notes(state, account_id, &rows).await?;
    let next_cursor = has_more
        .then(|| rows.last())
        .flatten()
        .map(|row| cursor::encode(row.last_ts, &row.thread_id));

    Ok(ThreadsResponse {
        threads: rows
            .into_iter()
            .map(|row| {
                let agent_note = notes.get(&row.thread_id).cloned();
                row.into_summary(agent_note)
            })
            .collect(),
        next_cursor,
    })
}

/// `agent_note` is "exactly the string the mail row renders under the snippet"
/// (`API.md` §2), so it is *stored* by whichever run produced it rather than
/// composed here from a title and a body. P2 has no runs, so this is null for
/// every thread; P5 writes `data.agent_note` and this starts returning it
/// without the endpoint changing shape.
async fn agent_notes(
    state: &AppState,
    account_id: Uuid,
    rows: &[ThreadRow],
) -> ApiResult<std::collections::HashMap<String, String>> {
    if rows.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let thread_ids: Vec<String> = rows.iter().map(|row| row.thread_id.clone()).collect();

    let found: Vec<(String, String)> = sqlx::query_as(
        "select distinct on (data->>'thread_id') data->>'thread_id', data->>'agent_note' \
           from feed_items \
          where account_id = $1 \
            and status = 'new' \
            and data->>'agent_note' is not null \
            and data->>'thread_id' = any($2) \
          order by data->>'thread_id', created_at desc",
    )
    .bind(account_id)
    .bind(&thread_ids)
    .fetch_all(&state.pool)
    .await?;

    Ok(found.into_iter().collect())
}

/// The mailbox list: the whitelist in `API.md` §2's order, then user labels.
async fn visible_mailboxes(state: &AppState, account_id: Uuid) -> ApiResult<Vec<Mailbox>> {
    let labels: Vec<(String, String, String)> = sqlx::query_as(
        "select label_id, coalesce(name, label_id), coalesce(type, 'user') \
           from labels where account_id = $1",
    )
    .bind(account_id)
    .fetch_all(&state.pool)
    .await?;

    // `unread`/`total` count **threads**, not messages, and only inside the
    // synced window. They are not Gmail's totals.
    let counts: Vec<(String, i64, i64)> = sqlx::query_as(
        "select tl.label_id, \
                count(*)::bigint, \
                count(*) filter (where t.unread)::bigint \
           from thread_labels tl \
           join threads t \
             on t.account_id = tl.account_id and t.thread_id = tl.thread_id \
          where tl.account_id = $1 \
          group by tl.label_id",
    )
    .bind(account_id)
    .fetch_all(&state.pool)
    .await?;
    let counts: std::collections::HashMap<String, (i64, i64)> = counts
        .into_iter()
        .map(|(label, total, unread)| (label, (total, unread)))
        .collect();

    let count_for = |label_id: &str| counts.get(label_id).copied().unwrap_or((0, 0));

    let mut out = Vec::with_capacity(labels.len());

    // Gmail's own system labels have `name == id` (`CATEGORY_PERSONAL`), which
    // is not a thing to show a person. Map them, in the contract's order.
    for (id, name) in SYSTEM_MAILBOXES {
        if !labels.iter().any(|(label_id, _, _)| label_id == id) {
            continue;
        }
        let (total, unread) = count_for(id);
        out.push(Mailbox {
            id: id.to_owned(),
            name: name.to_owned(),
            kind: "system".to_owned(),
            unread,
            total,
        });
    }

    // Then user labels, `name` passed through from Gmail verbatim.
    let mut user: Vec<(String, String)> = labels
        .iter()
        .filter(|(_, _, kind)| kind == "user")
        .filter(|(_, name, _)| !name.starts_with(HIDDEN_PREFIX))
        .map(|(id, name, _)| (id.clone(), name.clone()))
        .collect();
    // Ordered by name, by **raw byte value** rather than locale collation: a
    // locale-aware sort would let the phone and the server disagree about the
    // order of `[Notion]` and `Éclair`.
    user.sort_by(|(_, left), (_, right)| left.as_bytes().cmp(right.as_bytes()));

    for (id, name) in user {
        let (total, unread) = count_for(&id);
        out.push(Mailbox {
            id,
            name,
            kind: "user".to_owned(),
            unread,
            total,
        });
    }

    Ok(out)
}

/// The label a thread is filed under, for the detail footer.
///
/// JUDGEMENT: a user label wins over a system one. A thread is always in
/// `INBOX` and some `CATEGORY_*`, so showing those would make the footer say
/// "Inbox" for every thread; the label the user filed it under is the one that
/// carries information. Ties break on raw byte order, like the mailbox list.
async fn mailbox_name_for(
    state: &AppState,
    account_id: Uuid,
    thread_id: &str,
) -> ApiResult<String> {
    let visible = visible_mailboxes(state, account_id).await?;
    let filed: Vec<String> = sqlx::query_scalar(
        "select label_id from thread_labels where account_id = $1 and thread_id = $2",
    )
    .bind(account_id)
    .bind(thread_id)
    .fetch_all(&state.pool)
    .await?;

    let pick = |kind: &str| {
        visible
            .iter()
            .filter(|mailbox| mailbox.kind == kind)
            .find(|mailbox| filed.iter().any(|label| label == &mailbox.id))
            .map(|mailbox| mailbox.name.clone())
    };

    Ok(pick("user").or_else(|| pick("system")).unwrap_or_default())
}

/// Resolve our stored attachment to Gmail's `attachmentId` and fetch the bytes.
///
/// `format=raw` - the format the sync uses, because it is the only one our MIME
/// parser can see - carries no `attachmentId` at all, so the mapping happens
/// here, on demand, against `format=full`. backend/DECISIONS.md D14.
async fn fetch_attachment_bytes(
    state: &AppState,
    account_id: Uuid,
    gmail_id: &str,
    stored: &StoredAttachment,
) -> ApiResult<Vec<u8>> {
    let client = state.gmail.client_for(account_id);
    let message = client.get_message_full(gmail_id).await.map_err(upstream)?;

    let matched = message.flat_parts().into_iter().find(|part| {
        let by_name = part.filename.as_deref().is_some_and(|filename| {
            !filename.is_empty() && filename == stored.name && part.body.size == stored.size_bytes
        });
        let by_content_id = match (&stored.content_id, part.header("content-id")) {
            (Some(ours), Some(theirs)) => {
                crate::mail::html::normalise_content_id(ours)
                    == crate::mail::html::normalise_content_id(theirs)
            }
            _ => false,
        };
        by_name || by_content_id
    });

    let Some(part) = matched else {
        // Gmail still has the message but not this part: the user deleted the
        // attachment, or the message was replaced.
        return Err(ApiError::not_found());
    };

    if part.body.size > MAX_ATTACHMENT_BYTES {
        return Err(ApiError::payload_too_large());
    }

    // Small parts arrive inline; large ones need a second call.
    if let Some(data) = &part.body.data {
        return crate::gmail::client::decode_base64url(data)
            .ok_or_else(|| ApiError::internal("attachment body", &"not base64url"));
    }
    let Some(attachment_id) = &part.body.attachment_id else {
        return Err(ApiError::not_found());
    };
    client
        .get_attachment(gmail_id, attachment_id)
        .await
        .map_err(upstream)
}

/// Gmail failures become the right code, never a 500.
fn upstream(error: GmailError) -> ApiError {
    match error {
        GmailError::NotFound => ApiError::not_found(),
        GmailError::NeedsReauth => ApiError::needs_reauth(),
        GmailError::Upstream(detail) => {
            tracing::warn!(%detail, "gmail failed after retries");
            ApiError::upstream_unavailable()
        }
    }
}

/// The RFC 5987 `attr-char` set; everything else is percent-encoded.
const ATTR_CHAR: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'!')
    .remove(b'#')
    .remove(b'$')
    .remove(b'&')
    .remove(b'+')
    .remove(b'-')
    .remove(b'.')
    .remove(b'^')
    .remove(b'_')
    .remove(b'`')
    .remove(b'|')
    .remove(b'~');

/// `attachment; filename="…"; filename*=UTF-8''…`
///
/// Both forms: the quoted one for clients that ignore RFC 5987, the encoded one
/// for the real name. A filename cannot inject a header - every byte outside
/// the printable ASCII range becomes `_` in the fallback and a percent-escape in
/// the encoded form.
#[must_use]
pub fn content_disposition(name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_graphic() && character != '"' && character != '\\' {
                character
            } else if character == ' ' {
                ' '
            } else {
                '_'
            }
        })
        .collect();
    let fallback = if fallback.trim().is_empty() {
        "attachment".to_owned()
    } else {
        fallback
    };
    let encoded = percent_encoding::utf8_percent_encode(name, ATTR_CHAR);
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

/// ISO-8601 UTC, second precision, always `Z`-suffixed (`API.md` §0). Never
/// milliseconds: the iOS side decodes with a fixed formatter.
#[must_use]
pub fn wire_ts(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

// ------------------------------------------------------------- row types --

#[derive(Debug, sqlx::FromRow)]
struct ThreadRow {
    thread_id: String,
    subject: String,
    snippet: String,
    from_name: String,
    from_email: String,
    last_ts: DateTime<Utc>,
    unread: bool,
    msg_count: i32,
}

impl ThreadRow {
    fn into_summary(self, agent_note: Option<String>) -> ThreadSummary {
        ThreadSummary {
            id: self.thread_id,
            subject: self.subject,
            snippet: self.snippet,
            from_name: self.from_name,
            from_email: self.from_email,
            ts: wire_ts(self.last_ts),
            unread: self.unread,
            msg_count: self.msg_count,
            agent_note,
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct MessageRow {
    id: i64,
    gmail_id: String,
    from_name: Option<String>,
    from_email: Option<String>,
    to_json: serde_json::Value,
    cc_json: serde_json::Value,
    internal_ts: Option<DateTime<Utc>>,
    body_text: Option<String>,
    body_html: Option<String>,
    label_ids: Vec<String>,
}

impl MessageRow {
    fn into_view(self, attachments: Vec<AttachmentView>) -> MessageView {
        MessageView {
            gmail_id: self.gmail_id,
            from_name: self.from_name.unwrap_or_default(),
            from_email: self.from_email.unwrap_or_default(),
            to: string_list(&self.to_json),
            cc: string_list(&self.cc_json),
            // A message with no timestamp at all cannot happen after ingest, but
            // the column is nullable, so the wire never sees a missing field.
            ts: wire_ts(self.internal_ts.unwrap_or_else(Utc::now)),
            body_text: self.body_text.unwrap_or_default(),
            body_html: self.body_html,
            label_ids: self.label_ids,
            attachments,
        }
    }
}

fn string_list(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|entry| entry.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, sqlx::FromRow)]
struct AttachmentRow {
    message_id: i64,
    att_id: String,
    name: String,
    mime: String,
    size_bytes: i64,
    inline: bool,
}

#[derive(Debug, sqlx::FromRow)]
struct StoredAttachment {
    #[allow(dead_code)]
    att_id: String,
    name: String,
    mime: String,
    size_bytes: i64,
    content_id: Option<String>,
}

#[cfg(test)]
mod tests;
