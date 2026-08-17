//! Everything ingest writes. One module, so the shape of a stored message is
//! defined in exactly one place.
//!
//! Every write is an **upsert** keyed on `(account_id, gmail_id)`. That is what
//! makes a crashed sync safe to re-run and a duplicate job delivery a no-op
//! rather than a second copy of the mailbox (edge cases 3 and 4).

use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    gmail::types::{GmailMessage, Label},
    mail::{html, parse::ParsedMessage},
};

/// Gmail's own flag for "not yet read". `API.md` §2 counts a thread unread when
/// **any** message in it carries this.
pub const UNREAD_LABEL: &str = "UNREAD";

/// Store the label list. Kept whole - including `[Gmail]`-prefixed ones, which
/// the API hides rather than the sync dropping, so a later phase can change its
/// mind without a re-sync.
///
/// # Errors
/// Returns an error if the write fails.
pub async fn upsert_labels(pool: &PgPool, account_id: Uuid, labels: &[Label]) -> Result<usize> {
    let mut tx = pool.begin().await?;
    for label in labels {
        sqlx::query(
            "insert into labels (account_id, label_id, name, type) values ($1, $2, $3, $4) \
             on conflict (account_id, label_id) do update set \
                 name = excluded.name, type = excluded.type",
        )
        .bind(account_id)
        .bind(&label.id)
        .bind(label.name.as_deref().unwrap_or(&label.id))
        .bind(label.kind.as_deref().unwrap_or("user"))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(labels.len())
}

/// A message as it goes into the database: whatever we could parse, plus what
/// Gmail told us regardless.
#[derive(Debug, Clone)]
pub struct IngestRow {
    pub gmail_id: String,
    pub thread_id: String,
    pub history_id: Option<i64>,
    pub internal_ts: Option<DateTime<Utc>>,
    pub from_name: String,
    pub from_email: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub snippet: String,
    pub body_text: String,
    pub body_html: Option<String>,
    pub label_ids: Vec<String>,
    pub attachments: Vec<crate::mail::parse::ParsedAttachment>,
}

impl IngestRow {
    /// A fully parsed message.
    #[must_use]
    pub fn parsed(message: &GmailMessage, parsed: ParsedMessage) -> Self {
        Self {
            gmail_id: message.id.clone(),
            thread_id: message
                .thread_id
                .clone()
                .unwrap_or_else(|| message.id.clone()),
            history_id: message.history(),
            // EDGE (clock skew): Gmail's own receipt time wins. The `Date`
            // header is the *sender's* clock and is routinely wrong, so it is
            // only a fallback for a message Gmail gave us no `internalDate` for.
            internal_ts: message.internal_ts().or(parsed.date),
            from_name: parsed.from_name,
            from_email: parsed.from_email,
            to: parsed.to,
            cc: parsed.cc,
            subject: parsed.subject,
            snippet: crate::mail::parse::snippet(&parsed.body_text),
            body_text: parsed.body_text,
            body_html: parsed.body_html,
            label_ids: message.label_ids.clone(),
            attachments: parsed.attachments,
        }
    }

    /// A message whose MIME we could not parse. PLAN.md §Gmail sync 2: a parse
    /// failure writes a metadata-only row and the sync carries on. Losing one
    /// body is survivable; losing the thread it belongs to is not.
    #[must_use]
    pub fn metadata_only(message: &GmailMessage) -> Self {
        let snippet = message
            .snippet
            .as_deref()
            .map(html::decode_entities)
            .unwrap_or_default();
        Self {
            gmail_id: message.id.clone(),
            thread_id: message
                .thread_id
                .clone()
                .unwrap_or_else(|| message.id.clone()),
            history_id: message.history(),
            internal_ts: message.internal_ts(),
            from_name: String::new(),
            from_email: String::new(),
            to: Vec::new(),
            cc: Vec::new(),
            subject: String::new(),
            snippet: crate::mail::parse::snippet(&snippet),
            // Never null on the wire, so never null here either.
            body_text: String::new(),
            body_html: None,
            label_ids: message.label_ids.clone(),
            attachments: Vec::new(),
        }
    }
}

/// Write one message and its attachment metadata.
///
/// # Errors
/// Returns an error if the write fails.
pub async fn upsert_message(
    tx: &mut Transaction<'_, Postgres>,
    account_id: Uuid,
    row: &IngestRow,
) -> Result<i64> {
    let message_id: i64 = sqlx::query_scalar(
        "insert into messages ( \
             account_id, gmail_id, thread_id, history_id, internal_ts, from_name, from_email, \
             to_json, cc_json, subject, snippet, body_text, body_html, label_ids, \
             has_attachments, deleted_at) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, null) \
         on conflict (account_id, gmail_id) do update set \
             thread_id = excluded.thread_id, \
             history_id = excluded.history_id, \
             internal_ts = excluded.internal_ts, \
             from_name = excluded.from_name, \
             from_email = excluded.from_email, \
             to_json = excluded.to_json, \
             cc_json = excluded.cc_json, \
             subject = excluded.subject, \
             snippet = excluded.snippet, \
             body_text = excluded.body_text, \
             body_html = excluded.body_html, \
             label_ids = excluded.label_ids, \
             has_attachments = excluded.has_attachments, \
             deleted_at = null \
         returning id",
    )
    .bind(account_id)
    .bind(&row.gmail_id)
    .bind(&row.thread_id)
    .bind(row.history_id)
    .bind(row.internal_ts)
    .bind(&row.from_name)
    .bind(&row.from_email)
    .bind(serde_json::to_value(&row.to)?)
    .bind(serde_json::to_value(&row.cc)?)
    .bind(&row.subject)
    .bind(&row.snippet)
    .bind(&row.body_text)
    .bind(row.body_html.as_ref())
    .bind(&row.label_ids)
    .bind(!row.attachments.is_empty())
    .fetch_one(&mut **tx)
    .await?;

    // Attachment ids are a pure function of the message bytes, so a re-sync
    // upserts the same rows. Anything no longer present is deleted, which is
    // what keeps a re-parse from leaving orphans behind.
    let keep: Vec<String> = row
        .attachments
        .iter()
        .map(|attachment| attachment.att_id.clone())
        .collect();
    sqlx::query("delete from attachments where message_id = $1 and not (att_id = any($2))")
        .bind(message_id)
        .bind(&keep)
        .execute(&mut **tx)
        .await?;

    for attachment in &row.attachments {
        sqlx::query(
            "insert into attachments \
                 (message_id, att_id, name, mime, size_bytes, content_id, inline) \
             values ($1, $2, $3, $4, $5, $6, $7) \
             on conflict (message_id, att_id) do update set \
                 name = excluded.name, mime = excluded.mime, \
                 size_bytes = excluded.size_bytes, content_id = excluded.content_id, \
                 inline = excluded.inline",
        )
        .bind(message_id)
        .bind(&attachment.att_id)
        .bind(&attachment.name)
        .bind(&attachment.mime)
        .bind(attachment.size_bytes)
        .bind(attachment.content_id.as_ref())
        .bind(attachment.inline)
        .execute(&mut **tx)
        .await?;
    }

    Ok(message_id)
}

/// Recompute one thread's rollup from its messages.
///
/// The list endpoint reads `threads`/`thread_labels` and never aggregates, so
/// this is where "newest message wins" and "unread if any message is unread"
/// are decided - once, here, rather than in three query sites.
///
/// # Errors
/// Returns an error if the write fails.
pub async fn refresh_thread(
    tx: &mut Transaction<'_, Postgres>,
    account_id: Uuid,
    thread_id: &str,
) -> Result<()> {
    let updated: Option<(DateTime<Utc>, bool)> = sqlx::query_as(
        "with live as ( \
             select * from messages \
              where account_id = $1 and thread_id = $2 and deleted_at is null \
         ), \
         newest as ( \
             select subject, snippet, from_name, from_email \
               from live order by internal_ts desc nulls last, gmail_id desc limit 1 \
         ), \
         rolled as ( \
             select count(*)::int as msg_count, \
                    coalesce(bool_or($3 = any(label_ids)), false) as unread, \
                    coalesce(max(internal_ts), now()) as last_ts \
               from live \
         ) \
         insert into threads (account_id, thread_id, last_ts, subject, snippet, \
                              from_name, from_email, unread, msg_count, updated_at) \
         select $1, $2, rolled.last_ts, \
                coalesce(newest.subject, ''), coalesce(newest.snippet, ''), \
                coalesce(newest.from_name, ''), coalesce(newest.from_email, ''), \
                rolled.unread, rolled.msg_count, now() \
           from rolled left join newest on true \
          where rolled.msg_count > 0 \
         on conflict (account_id, thread_id) do update set \
             last_ts = excluded.last_ts, subject = excluded.subject, \
             snippet = excluded.snippet, from_name = excluded.from_name, \
             from_email = excluded.from_email, unread = excluded.unread, \
             msg_count = excluded.msg_count, updated_at = now() \
         returning last_ts, unread",
    )
    .bind(account_id)
    .bind(thread_id)
    .bind(UNREAD_LABEL)
    .fetch_optional(&mut **tx)
    .await?;

    let Some((last_ts, unread)) = updated else {
        // Every message in the thread is gone: so is the thread.
        sqlx::query("delete from threads where account_id = $1 and thread_id = $2")
            .bind(account_id)
            .bind(thread_id)
            .execute(&mut **tx)
            .await?;
        return Ok(());
    };

    // The label set is the union across the thread's messages, which is what
    // makes a thread appear in every mailbox any of its messages is filed under.
    sqlx::query("delete from thread_labels where account_id = $1 and thread_id = $2")
        .bind(account_id)
        .bind(thread_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "insert into thread_labels (account_id, thread_id, label_id, last_ts, unread) \
         select distinct $1, $2, label, $3, $4 \
           from messages, unnest(label_ids) as label \
          where account_id = $1 and thread_id = $2 and deleted_at is null \
         on conflict (account_id, thread_id, label_id) do update set \
             last_ts = excluded.last_ts, unread = excluded.unread",
    )
    .bind(account_id)
    .bind(thread_id)
    .bind(last_ts)
    .bind(unread)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// Record the `historyId` read **before** the listing started.
///
/// # Errors
/// Returns an error if the write fails.
pub async fn record_history_id(
    pool: &PgPool,
    account_id: Uuid,
    history_id: Option<i64>,
) -> Result<()> {
    sqlx::query(
        "insert into sync_state (account_id, last_history_id, updated_at) \
         values ($1, $2, now()) \
         on conflict (account_id) do update set \
             last_history_id = coalesce(excluded.last_history_id, sync_state.last_history_id), \
             updated_at = now()",
    )
    .bind(account_id)
    .bind(history_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Append an audit row. Used for every message we could not parse or fetch, so
/// "the sync carried on" is a claim with evidence behind it.
///
/// # Errors
/// Returns an error if the write fails.
pub async fn audit(
    pool: &PgPool,
    account_id: Uuid,
    action: &str,
    subject: serde_json::Value,
) -> Result<()> {
    sqlx::query(
        "insert into audit_log (account_id, actor, action, subject) values ($1, 'system', $2, $3)",
    )
    .bind(account_id)
    .bind(action)
    .bind(subject)
    .execute(pool)
    .await?;
    Ok(())
}
