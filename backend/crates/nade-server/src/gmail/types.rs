//! The slices of Gmail's REST responses we actually read.
//!
//! Deliberately lenient: every field that Gmail documents as optional is
//! `Option` or `#[serde(default)]`, because a missing field must degrade one
//! message rather than abort a sync. `historyId`, `internalDate` and `size` come
//! over the wire as **strings** holding 64-bit numbers, which is a classic place
//! to lose data by typing them as `u64`.

use serde::Deserialize;

/// `users.getProfile`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub email_address: String,
    #[serde(default)]
    pub messages_total: Option<i64>,
    /// Read **before** listing, so a message arriving mid-sync causes an overlap
    /// rather than a gap (PLAN.md §Gmail sync 2).
    #[serde(default)]
    pub history_id: Option<String>,
}

/// One entry of `users.labels.list`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Label {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    /// `system` or `user`.
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelsList {
    #[serde(default)]
    pub labels: Vec<Label>,
}

/// A `users.messages.list` entry: ids only, no content.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageRef {
    pub id: String,
    #[serde(default)]
    pub thread_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessagesList {
    #[serde(default)]
    pub messages: Vec<MessageRef>,
    #[serde(default)]
    pub next_page_token: Option<String>,
}

/// `users.threads.get?format=minimal` - Gmail's own answer to how many messages
/// a conversation has, which is the only authority on whether our copy of it is
/// whole.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GmailThread {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub messages: Vec<MessageRef>,
}

/// `users.messages.get`, in either `format=raw` or `format=full`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GmailMessage {
    pub id: String,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub label_ids: Vec<String>,
    #[serde(default)]
    pub snippet: Option<String>,
    #[serde(default)]
    pub history_id: Option<String>,
    /// Milliseconds since the epoch, as a string. Gmail's own receipt time -
    /// the sender's `Date` header is only a fallback (edge case 8, clock skew).
    #[serde(default)]
    pub internal_date: Option<String>,
    /// `format=raw` only: the whole RFC-822 message, base64url.
    #[serde(default)]
    pub raw: Option<String>,
    /// `format=full` only: the parsed part tree. We read it for one purpose -
    /// resolving a stored attachment to Gmail's `attachmentId` at download time.
    #[serde(default)]
    pub payload: Option<Payload>,
}

impl GmailMessage {
    /// `internalDate` as a UTC timestamp.
    #[must_use]
    pub fn internal_ts(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        let millis: i64 = self.internal_date.as_deref()?.trim().parse().ok()?;
        chrono::DateTime::from_timestamp_millis(millis)
    }

    /// `historyId` as a number, for `sync_state.last_history_id`.
    #[must_use]
    pub fn history(&self) -> Option<i64> {
        self.history_id.as_deref()?.trim().parse().ok()
    }

    /// Walk the whole part tree, depth first.
    #[must_use]
    pub fn flat_parts(&self) -> Vec<&Payload> {
        let mut out = Vec::new();
        if let Some(root) = &self.payload {
            collect_parts(root, &mut out);
        }
        out
    }
}

fn collect_parts<'a>(payload: &'a Payload, out: &mut Vec<&'a Payload>) {
    out.push(payload);
    for part in &payload.parts {
        collect_parts(part, out);
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Payload {
    #[serde(default)]
    pub part_id: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub headers: Vec<Header>,
    #[serde(default)]
    pub body: Body,
    #[serde(default)]
    pub parts: Vec<Payload>,
}

impl Payload {
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case(name))
            .map(|header| header.value.as_str())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Header {
    pub name: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Body {
    /// Present on parts big enough that Gmail stores them separately. This is
    /// the handle `users.messages.attachments.get` takes.
    #[serde(default)]
    pub attachment_id: Option<String>,
    #[serde(default)]
    pub size: i64,
    /// Present instead of `attachmentId` for small parts - already the bytes.
    #[serde(default)]
    pub data: Option<String>,
}

/// `users.history.list`. P2 only reads the `historyId` cursor; P3's incremental
/// sync walks these pages. It lives here now so the cursor P2 records has a
/// consumer that is already tested against a real response shape.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryList {
    #[serde(default)]
    pub history: Vec<HistoryRecord>,
    #[serde(default)]
    pub next_page_token: Option<String>,
    /// The cursor to store once the whole walk succeeds.
    #[serde(default)]
    pub history_id: Option<String>,
}

impl HistoryList {
    /// Every message id this page touches, in any of the four history types.
    #[must_use]
    pub fn touched_message_ids(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for record in &self.history {
            let groups = [
                &record.messages_added,
                &record.messages_deleted,
                &record.labels_added,
                &record.labels_removed,
            ];
            for group in groups {
                for entry in group.iter() {
                    let id = entry.message.id.clone();
                    if !id.is_empty() && !out.contains(&id) {
                        out.push(id);
                    }
                }
            }
        }
        out
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRecord {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub messages_added: Vec<HistoryMessage>,
    #[serde(default)]
    pub messages_deleted: Vec<HistoryMessage>,
    #[serde(default)]
    pub labels_added: Vec<HistoryMessage>,
    #[serde(default)]
    pub labels_removed: Vec<HistoryMessage>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryMessage {
    #[serde(default)]
    pub message: MessageStub,
    #[serde(default)]
    pub label_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageStub {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub thread_id: Option<String>,
}

/// `users.messages.attachments.get`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    #[serde(default)]
    pub size: i64,
    /// base64url, unpadded.
    #[serde(default)]
    pub data: String,
}

/// Google's standard error envelope, which is what tells 429-because-of-quota
/// apart from 403-because-you-are-not-allowed.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApiErrorEnvelope {
    #[serde(default)]
    pub error: ApiErrorBody,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApiErrorBody {
    #[serde(default)]
    pub code: i64,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub errors: Vec<ApiErrorDetail>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApiErrorDetail {
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub message: String,
}

impl ApiErrorEnvelope {
    /// The reasons Google uses for "you are going too fast", as opposed to the
    /// ones that mean "never".
    #[must_use]
    pub fn is_rate_limit(&self) -> bool {
        const RETRYABLE: [&str; 4] = [
            "rateLimitExceeded",
            "userRateLimitExceeded",
            "quotaExceeded",
            "backendError",
        ];
        self.error
            .errors
            .iter()
            .any(|detail| RETRYABLE.contains(&detail.reason.as_str()))
            || RETRYABLE.contains(&self.error.status.as_str())
    }

    #[must_use]
    pub fn parse(body: &[u8]) -> Self {
        serde_json::from_slice(body).unwrap_or_default()
    }
}

/// Will trying this again ever help?
///
/// **One classifier, two call sites.** `client::send` consults it for a whole
/// HTTP response and `sync` consults it for a `multipart/mixed` **sub**-response,
/// and the second is the one the first live sync got wrong: Gmail answers a
/// batch `200` and puts the per-message `429`s *inside* it, so the client's
/// retry loop never saw them and the sync treated a rate limit as "this message
/// does not exist". Anything not provably permanent is transient here, because
/// the cost of an unnecessary retry is a second of latency and the cost of a
/// wrong "permanent" is mail that is never fetched again.
///
/// `404`/`410` are neither: the message is gone and the caller handles that
/// before asking.
#[must_use]
pub fn is_transient(status: u16, body: &[u8]) -> bool {
    match status {
        // No sub-response carried our `Content-ID` at all. We know nothing
        // about this message, and "we know nothing" is never permission to
        // forget it.
        0 => true,
        // The request itself is wrong. Sending it again produces the same 400.
        400 => false,
        // The access token died mid-batch; `client::send` refreshes it and the
        // retry is then a different request.
        401 => true,
        // The ambiguous one: `rateLimitExceeded` means slow down,
        // `insufficientPermissions` means never.
        403 => ApiErrorEnvelope::parse(body).is_rate_limit(),
        404 | 410 => false,
        429 => true,
        code if (500..600).contains(&code) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_and_internal_date_survive_being_strings() {
        let message: GmailMessage = serde_json::from_str(
            r#"{"id":"18f2","threadId":"18f2","historyId":"18446744073709551","internalDate":"1755335524000"}"#,
        )
        .unwrap();
        assert_eq!(message.history(), Some(18_446_744_073_709_551));
        assert_eq!(
            message.internal_ts().unwrap().to_rfc3339(),
            "2025-08-16T09:12:04+00:00"
        );
    }

    /// EDGE: a message stripped to nothing but an id still deserialises. Gmail
    /// really does return this for some deleted-mid-flight messages.
    #[test]
    fn a_minimal_message_still_deserialises() {
        let message: GmailMessage = serde_json::from_str(r#"{"id":"x"}"#).unwrap();
        assert_eq!(message.id, "x");
        assert!(message.internal_ts().is_none());
        assert!(message.label_ids.is_empty());
        assert!(message.flat_parts().is_empty());
    }

    #[test]
    fn the_part_tree_is_walked_depth_first() {
        let message: GmailMessage = serde_json::from_str(
            r#"{"id":"x","payload":{"mimeType":"multipart/mixed","parts":[
                 {"mimeType":"multipart/alternative","parts":[
                   {"mimeType":"text/plain"},{"mimeType":"text/html"}]},
                 {"mimeType":"application/pdf","filename":"a.pdf",
                  "body":{"attachmentId":"ANGjdJ_qk","size":245760}}]}}"#,
        )
        .unwrap();
        let mimes: Vec<&str> = message
            .flat_parts()
            .iter()
            .filter_map(|part| part.mime_type.as_deref())
            .collect();
        assert_eq!(
            mimes,
            vec![
                "multipart/mixed",
                "multipart/alternative",
                "text/plain",
                "text/html",
                "application/pdf"
            ]
        );
        let pdf = message
            .flat_parts()
            .into_iter()
            .find(|part| part.filename.as_deref() == Some("a.pdf"))
            .unwrap();
        assert_eq!(pdf.body.attachment_id.as_deref(), Some("ANGjdJ_qk"));
        assert_eq!(pdf.body.size, 245_760);
    }

    #[test]
    fn rate_limit_reasons_are_told_apart_from_plain_denial() {
        let throttled = ApiErrorEnvelope::parse(
            br#"{"error":{"code":403,"message":"Rate Limit Exceeded",
                 "errors":[{"domain":"usageLimits","reason":"rateLimitExceeded"}]}}"#,
        );
        assert!(throttled.is_rate_limit());

        let denied = ApiErrorEnvelope::parse(
            br#"{"error":{"code":403,"message":"Insufficient Permission",
                 "errors":[{"domain":"global","reason":"insufficientPermissions"}]}}"#,
        );
        assert!(!denied.is_rate_limit());

        // Garbage never panics and is never mistaken for a throttle.
        assert!(!ApiErrorEnvelope::parse(b"<html>502</html>").is_rate_limit());
        assert!(!ApiErrorEnvelope::parse(b"").is_rate_limit());
    }

    /// The classification the first live sync got wrong. The body here is the
    /// **verbatim** payload Gmail returned inside a `200` batch response, 91
    /// times, while `sync` skipped each of those messages and then advanced the
    /// cursor past them.
    #[test]
    fn a_429_inside_a_batch_is_transient_and_a_400_is_not() {
        let live = br#"{"error":{"code":429,
             "message":"Too many concurrent requests for user.",
             "errors":[{"message":"Too many concurrent requests for user.",
                        "domain":"global","reason":"rateLimitExceeded"}],
             "status":"RESOURCE_EXHAUSTED"}}"#;
        assert!(
            is_transient(429, live),
            "a 429 must be retried, never skipped"
        );

        assert!(is_transient(500, b""), "a backend error is transient");
        assert!(is_transient(503, b""));
        assert!(
            is_transient(0, b""),
            "no sub-response means we know nothing"
        );
        assert!(is_transient(401, b""), "the token can be refreshed");
        assert!(is_transient(
            403,
            br#"{"error":{"code":403,"errors":[{"reason":"userRateLimitExceeded"}]}}"#
        ));

        assert!(
            !is_transient(400, b""),
            "a malformed request stays malformed"
        );
        assert!(!is_transient(
            403,
            br#"{"error":{"code":403,"errors":[{"reason":"insufficientPermissions"}]}}"#
        ));
        assert!(
            !is_transient(404, b""),
            "a deleted message is Gone, not failed"
        );
        assert!(!is_transient(410, b""));
        assert!(!is_transient(200, b""));
        // Garbage bodies never panic and never become a retry loop.
        assert!(!is_transient(403, b"<html>nope"));
    }

    #[test]
    fn headers_are_matched_case_insensitively() {
        let payload: Payload =
            serde_json::from_str(r#"{"headers":[{"name":"Content-ID","value":"<logo@x>"}]}"#)
                .unwrap();
        assert_eq!(payload.header("content-id"), Some("<logo@x>"));
        assert_eq!(payload.header("CONTENT-ID"), Some("<logo@x>"));
        assert_eq!(payload.header("missing"), None);
    }
}
