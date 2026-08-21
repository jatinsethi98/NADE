//! `POST /v1/webhooks/gmail` - Gmail's Pub/Sub push.
//!
//! Mounted on the **public** router, because Pub/Sub cannot present a bearer.
//! It is not open: [`oidc::Verifier`] checks the OIDC token in full before the
//! body is looked at.
//!
//! The handler answers `204` fast and does the work on the job queue. Anything
//! else makes Pub/Sub retry, which is why an unreadable body from an
//! already-verified sender is still a `204`.

pub mod oidc;

use axum::{extract::State, http::HeaderMap, http::StatusCode};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::json;

use crate::{
    error::ApiResult,
    jobs::Queue,
    state::AppState,
    sync::{incremental, store},
};

/// Pub/Sub's push envelope.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PushEnvelope {
    #[serde(default)]
    message: PushMessage,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PushMessage {
    #[serde(default)]
    data: String,
    #[serde(default)]
    message_id: Option<String>,
}

/// Gmail's own notification, inside `message.data`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GmailNotification {
    /// Required. Gmail always sends it, and treating it as optional let any
    /// JSON object at all decode as a notification - including one that says
    /// nothing about mail.
    email_address: String,
    /// A JSON **number** here, though the REST API returns history ids as
    /// strings. Both are accepted, because getting this wrong is a `500` on
    /// every push for ever.
    #[serde(default, deserialize_with = "number_or_string")]
    history_id: Option<u64>,
}

fn number_or_string<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Number(u64),
        Text(String),
    }
    Ok(match Option::<Either>::deserialize(deserializer)? {
        Some(Either::Number(value)) => Some(value),
        Some(Either::Text(text)) => text.parse().ok(),
        None => None,
    })
}

/// Gmail push.
///
/// # Errors
/// [`crate::error::ApiError::unauthorized`] for every forgery class, and
/// `502` when Google's key set is cold and unreachable. A database failure is a
/// `500`, and Pub/Sub retries it - which is correct, because the notification
/// really has not been recorded.
pub async fn gmail(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> ApiResult<StatusCode> {
    // Verification comes first, and takes `Bytes` rather than a typed
    // extractor, so nothing the caller controls beyond the token can influence
    // whether we authenticate.
    let authorization = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    state.push.verify(authorization).await?;

    // From here the sender is Google. An unreadable body therefore means the
    // shape changed, and a `400` would make Pub/Sub retry a poison message for
    // ever - so it is audited and acknowledged. With no address to route by,
    // the audit row belongs to no account.
    let Some(delivery) = decode(&body) else {
        tracing::warn!("a verified push carried a body we could not read");
        let _ = audit_unrouted(
            &state,
            "gmail_push_unreadable",
            json!({ "bytes": body.len() }),
        )
        .await;
        return Ok(StatusCode::NO_CONTENT);
    };

    // The notification names its mailbox, so it routes itself: every watch on
    // the topic lands here, and the address decides whose cursor it drives.
    // Case is ignored because Gmail echoes whatever the consent typed.
    let addressed = &delivery.notification.email_address;
    let account_id: Option<uuid::Uuid> = sqlx::query_scalar(
        "select id from accounts where lower(email) = lower($1) \
          order by created_at, id limit 1",
    )
    .bind(addressed)
    .fetch_optional(&state.pool)
    .await?;
    let Some(account_id) = account_id else {
        // A valid delivery for a mailbox this server does not hold - another
        // watch on the same topic, or an account that has not consented yet.
        // Acknowledged, and audited to no account, so it leaves evidence
        // without inventing an owner.
        tracing::warn!(%addressed, "a push for an unknown mailbox was ignored");
        audit_unrouted(
            &state,
            "gmail_push_unknown_mailbox",
            json!({ "email_address": addressed }),
        )
        .await?;
        return Ok(StatusCode::NO_CONTENT);
    };

    // The notification's own `historyId` is deliberately NEVER written to
    // `sync_state`. The walk is driven only by our own cursor, which is what
    // makes Pub/Sub's at-least-once, out-of-order delivery irrelevant: a push
    // for 100 arriving before one for 99 triggers exactly the same walk.
    sqlx::query(
        "insert into sync_state (account_id, last_webhook_at, updated_at) \
         values ($1, now(), now()) \
         on conflict (account_id) do update set last_webhook_at = now(), updated_at = now()",
    )
    .bind(account_id)
    .execute(&state.pool)
    .await?;

    let queue = Queue::new(state.pool.clone(), state.config.jobs.clone());
    let dedupe = format!("{}:{}", incremental::KIND, account_id);
    let enqueued = queue
        .enqueue_unique(
            incremental::KIND,
            &json!({
                "account_id": account_id,
                "source": "webhook",
                "history_id": delivery.notification.history_id,
                "message_id": delivery.message_id,
            }),
            None,
            &dedupe,
        )
        .await?;

    if enqueued.is_none() {
        // A walk is already pending or running, so this notification would be
        // silently dropped. Record it: the running walk takes the flag before
        // it finishes, and re-reading the cursor could never have seen this
        // change, because we never wrote the notification's `historyId`.
        store::request_rerun(&state.pool, account_id).await?;
    }

    // Both writes stay inside the request rather than a `tokio::spawn`. A
    // spawned task can be dropped by graceful shutdown *after* Pub/Sub has been
    // told `204`, losing the notification with no trace anywhere.
    Ok(StatusCode::NO_CONTENT)
}

/// An audit row that belongs to no account (`audit_log.account_id` is nullable
/// for exactly this). [`store::audit`] requires an owner; a push we could not
/// route has none, and inventing one would file evidence under the wrong user.
async fn audit_unrouted(
    state: &AppState,
    action: &str,
    subject: serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query("insert into audit_log (actor, action, subject) values ('system', $1, $2)")
        .bind(action)
        .bind(subject)
        .execute(&state.pool)
        .await?;
    Ok(())
}

/// Decode `message.data`.
///
/// Pub/Sub's own documentation says base64, and Google's Gmail push guide shows
/// base64**url**. Real deliveries have been seen both ways, so asserting either
/// is how a notification gets permanently acknowledged as malformed by the rule
/// above. `decode_base64url` already accepts url-safe, standard, padded and
/// unpadded, so it is reused here - the trap being that its *name* suggests it
/// does not.
fn decode(body: &[u8]) -> Option<Delivery> {
    let envelope: PushEnvelope = serde_json::from_slice(body).ok()?;
    let raw = crate::gmail::client::decode_base64url(&envelope.message.data).or_else(|| {
        base64::engine::general_purpose::STANDARD
            .decode(envelope.message.data.trim())
            .ok()
    })?;
    let notification: GmailNotification = serde_json::from_slice(&raw).ok()?;
    if notification.email_address.trim().is_empty() {
        return None;
    }
    Some(Delivery {
        notification,
        message_id: envelope.message.message_id,
    })
}

/// One verified, decoded delivery.
#[derive(Debug)]
struct Delivery {
    notification: GmailNotification,
    /// Pub/Sub's own id. Logged and journalled, never used for dedupe: the work
    /// is cursor-driven and idempotent at every layer, so a redelivery produces
    /// an empty history page. A `seen_message_ids` table would be a second
    /// thing to garbage-collect and a second thing to get wrong.
    message_id: Option<String>,
}

#[cfg(test)]
mod tests;
