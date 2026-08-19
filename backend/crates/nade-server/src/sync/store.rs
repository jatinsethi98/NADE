//! Everything ingest writes. One module, so the shape of a stored message is
//! defined in exactly one place.
//!
//! Every write is an **upsert** keyed on `(account_id, gmail_id)`. That is what
//! makes a crashed sync safe to re-run and a duplicate job delivery a no-op
//! rather than a second copy of the mailbox (edge cases 3 and 4).

use std::borrow::Cow;

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

/// Anything on its way into a `text` column, with the bytes PostgreSQL refuses
/// removed.
///
/// **This is the third time this bug has been written.** A `NUL` in a body
/// killed the first live sync; the fix scrubbed `body_text`, and `body_html` -
/// a sibling derived from the same source - killed it again. The third was
/// `snippet`: `metadata_only` runs Gmail's snippet through
/// `html::decode_entities`, and `&#0;` decodes to a literal `NUL`, so a
/// message whose MIME failed to parse could take the whole statement down.
///
/// So the rule now lives at the **writer** rather than beside each value. Every
/// text column this function binds goes through here, including the ones a
/// later phase adds, and no future constructor has to remember.
///
/// Borrowed for the overwhelming majority, which carry nothing to remove.
fn storable(text: &str) -> Cow<'_, str> {
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        Cow::Owned(
            text.chars()
                .filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
                .collect(),
        )
    } else {
        Cow::Borrowed(text)
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
    .bind(storable(&row.from_name))
    .bind(storable(&row.from_email))
    .bind(serde_json::to_value(&row.to)?)
    .bind(serde_json::to_value(&row.cc)?)
    .bind(storable(&row.subject))
    .bind(storable(&row.snippet))
    .bind(storable(&row.body_text))
    .bind(row.body_html.as_deref().map(storable))
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
                 (message_id, att_id, name, mime, size_bytes, content_id, inline, ordinal) \
             values ($1, $2, $3, $4, $5, $6, $7, $8) \
             on conflict (message_id, att_id) do update set \
                 name = excluded.name, mime = excluded.mime, \
                 size_bytes = excluded.size_bytes, content_id = excluded.content_id, \
                 inline = excluded.inline, ordinal = excluded.ordinal",
        )
        .bind(message_id)
        .bind(&attachment.att_id)
        .bind(&attachment.name)
        .bind(&attachment.mime)
        .bind(attachment.size_bytes)
        .bind(attachment.content_id.as_ref())
        .bind(attachment.inline)
        .bind(attachment.ordinal)
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
    let mut tx = pool.begin().await?;
    advance_history_id(&mut tx, account_id, history_id).await?;
    tx.commit().await?;
    Ok(())
}

/// Move the cursor forward, **never backward**, and stamp `last_checked_at`.
///
/// `greatest` is the compare-and-set, and it is what makes replay safe. Pub/Sub
/// delivers at-least-once and out of order, a crashed walk re-covers ground it
/// already committed, and a stale job can finish after a newer one - all three
/// offer a value less than or equal to the stored one, and all three are
/// discarded here rather than in three call sites. PostgreSQL's `greatest`
/// ignores nulls, so the first write and the null-cursor case fall out for
/// free.
///
/// # Errors
/// Returns an error if the write fails.
pub async fn advance_history_id(
    tx: &mut Transaction<'_, Postgres>,
    account_id: Uuid,
    history_id: Option<i64>,
) -> Result<()> {
    sqlx::query(
        "insert into sync_state (account_id, last_history_id, last_checked_at, updated_at) \
         values ($1, $2, now(), now()) \
         on conflict (account_id) do update set \
             last_history_id = greatest(sync_state.last_history_id, excluded.last_history_id), \
             last_checked_at = now(), \
             updated_at = now()",
    )
    .bind(account_id)
    .bind(history_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Stamp `last_checked_at` without touching the cursor.
///
/// An empty history page is a check that changed nothing, not a check that did
/// not happen - so it must still hold off the 30-minute poll.
///
/// # Errors
/// Returns an error if the write fails.
pub async fn touch_checked(tx: &mut Transaction<'_, Postgres>, account_id: Uuid) -> Result<()> {
    advance_history_id(tx, account_id, None).await
}

/// Apply one message's net label delta.
///
/// The result is `(existing ∪ added) \ removed`, de-duplicated and sorted, so
/// applying the same page twice is the same as applying it once - which is the
/// whole reason a redelivered notification is harmless. Sorting is free: nothing
/// reads `label_ids` in order (`refresh_thread` unnests it and mailbox
/// membership reads `thread_labels`).
///
/// **`deleted_at` is cleared.** Trash and un-trash both arrive as label events,
/// never as `messagesDeleted`, and until now only a full upsert cleared the
/// column - so a message the reconciliation sweep soft-deleted and the user
/// then restored would get correct labels and stay invisible forever.
///
/// Returns the thread id, or `None` when we do not hold the message - a label
/// change for mail outside the cached window is counted and skipped rather than
/// fetched, because re-reading fifty messages in the Gmail UI would otherwise
/// cost the entire per-second quota budget in `UNREAD` flips.
///
/// # Errors
/// Returns an error if the write fails.
pub async fn apply_label_delta(
    tx: &mut Transaction<'_, Postgres>,
    account_id: Uuid,
    gmail_id: &str,
    added: &[String],
    removed: &[String],
) -> Result<Option<String>> {
    let thread_id: Option<String> = sqlx::query_scalar(
        "update messages \
            set label_ids = coalesce(( \
                    select array_agg(distinct label order by label) \
                      from unnest(label_ids || $3::text[]) as label \
                     where not (label = any($4::text[]))), '{}'), \
                deleted_at = null \
          where account_id = $1 and gmail_id = $2 \
         returning thread_id",
    )
    .bind(account_id)
    .bind(gmail_id)
    .bind(added)
    .bind(removed)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(thread_id)
}

/// Soft-delete a message Gmail says is gone.
///
/// The thread id comes from **our** row: a `messagesDeleted` history record
/// does not reliably carry one, and every reader already filters
/// `deleted_at is null`, so re-deleting an already-deleted row is a no-op.
///
/// # Errors
/// Returns an error if the write fails.
pub async fn soft_delete_message(
    tx: &mut Transaction<'_, Postgres>,
    account_id: Uuid,
    gmail_id: &str,
) -> Result<Option<String>> {
    let thread_id: Option<String> = sqlx::query_scalar(
        "update messages set deleted_at = coalesce(deleted_at, now()) \
          where account_id = $1 and gmail_id = $2 \
         returning thread_id",
    )
    .bind(account_id)
    .bind(gmail_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(thread_id)
}

/// Mark that a reconciliation sweep is owed, carrying the cursor that expired.
///
/// Written **before the recovery re-sync begins**, and cleared only by a
/// completed sweep.
///
/// The ordering is the safety property, not a transaction boundary: the re-sync
/// durably commits a *fresh* cursor before returning, so a crash after it would
/// leave the retry asking `history.list` from the new cursor, getting a `200`
/// instead of the `404` that enters recovery — and nothing would ever
/// reconcile. Persisting the debt first means every one of those crash points
/// still owes the sweep.
///
/// # Errors
/// Returns an error if the write fails.
pub async fn owe_reconcile(
    tx: &mut Transaction<'_, Postgres>,
    account_id: Uuid,
    stale_cursor: Option<i64>,
) -> Result<()> {
    sqlx::query(
        "insert into sync_state (account_id, reconcile_after, updated_at) \
         values ($1, coalesce($2, 0), now()) \
         on conflict (account_id) do update set \
             reconcile_after = coalesce(excluded.reconcile_after, 0), updated_at = now()",
    )
    .bind(account_id)
    .bind(stale_cursor)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Clear the reconciliation debt. Called only when a sweep completed.
///
/// # Errors
/// Returns an error if the write fails.
pub async fn clear_reconcile(tx: &mut Transaction<'_, Postgres>, account_id: Uuid) -> Result<()> {
    sqlx::query(
        "update sync_state set reconcile_after = null, updated_at = now() where account_id = $1",
    )
    .bind(account_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Is a reconciliation owed?
///
/// # Errors
/// Returns an error if the read fails.
pub async fn reconcile_owed(pool: &PgPool, account_id: Uuid) -> Result<Option<i64>> {
    let owed: Option<Option<i64>> =
        sqlx::query_scalar("select reconcile_after from sync_state where account_id = $1")
            .bind(account_id)
            .fetch_optional(pool)
            .await?;
    Ok(owed.flatten())
}

/// Record that a notification arrived which no walk has covered yet.
///
/// Called when `enqueue_unique` finds a job already pending: the notification
/// is real, but the job that would have carried it was suppressed. Without this
/// the change waits for the 30-minute poll, because the walk's own cursor
/// cannot see it - the webhook deliberately never writes the notification's
/// `historyId`.
///
/// # Errors
/// Returns an error if the write fails.
pub async fn request_rerun(pool: &PgPool, account_id: Uuid) -> Result<()> {
    sqlx::query(
        "insert into sync_state (account_id, rerun_requested, updated_at) \
         values ($1, true, now()) \
         on conflict (account_id) do update set rerun_requested = true, updated_at = now()",
    )
    .bind(account_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Read the rerun flag and clear it in one statement.
///
/// Read-then-clear as two statements would drop a notification that landed
/// between them, which is the bug this whole mechanism exists to fix.
///
/// # Errors
/// Returns an error if the write fails.
pub async fn take_rerun_request(
    tx: &mut Transaction<'_, Postgres>,
    account_id: Uuid,
) -> Result<bool> {
    let requested: Option<bool> = sqlx::query_scalar(
        "update sync_state set rerun_requested = false \
          where account_id = $1 and rerun_requested \
         returning true",
    )
    .bind(account_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(requested.unwrap_or(false))
}

/// Append an audit row **inside a caller's transaction**, so a page's evidence
/// commits with the page rather than surviving a rollback.
///
/// # Errors
/// Returns an error if the write fails.
pub async fn audit_in(
    tx: &mut Transaction<'_, Postgres>,
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
    .execute(&mut **tx)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gmail::types::GmailMessage;

    /// The third instance of the bug that killed the live sync twice.
    ///
    /// `metadata_only` runs Gmail's own snippet through `decode_entities`, and
    /// `&#0;` decodes to a literal `NUL`. `parse::snippet` only collapses
    /// whitespace, and a `NUL` is not `White_Space`, so it survived into the
    /// insert and PostgreSQL rejected the whole statement — failing the job on
    /// every attempt, exactly as before.
    ///
    /// The guarantee now lives at the writer, so this asserts the writer's
    /// helper rather than one field's constructor.
    #[test]
    fn a_gmail_snippet_cannot_carry_a_nul_into_a_text_column() {
        let mut message = GmailMessage {
            id: "18f2a1b3c4d5e6f7".to_owned(),
            ..GmailMessage::default()
        };
        message.snippet = Some("Invoice&#0; attached&#1;".to_owned());

        let row = IngestRow::metadata_only(&message);
        assert!(
            row.snippet.contains('\u{0}'),
            "the entity decoder really does produce the byte Postgres refuses; \
             without that this test proves nothing"
        );

        let stored = storable(&row.snippet);
        assert!(
            !stored
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t'),
            "a control character reached a text column: {stored:?}"
        );
        assert_eq!(stored, "Invoice attached");
    }

    /// Every text column the writer binds, not just the one that broke last.
    #[test]
    fn the_scrub_is_the_writers_rule_and_covers_every_field() {
        for hostile in [
            "sub\u{0}ject",
            "na\u{1}me",
            "bo\u{7}dy",
            "\u{0}",
            "html\u{0}body",
        ] {
            let clean = storable(hostile);
            assert!(
                !clean.chars().any(char::is_control),
                "{hostile:?} survived as {clean:?}"
            );
        }

        // EDGE (the common case): nothing to remove means nothing allocated.
        assert!(matches!(
            storable("ordinary text\nwith a newline"),
            Cow::Borrowed(_)
        ));
        // Newline, carriage return and tab are legal in a text column and are
        // real content in a mail body.
        assert_eq!(storable("a\nb\tc\rd"), "a\nb\tc\rd");
        // EDGE (empty input / unicode).
        assert_eq!(storable(""), "");
        assert_eq!(storable("配送のお知らせ 🚀"), "配送のお知らせ 🚀");
    }
}
