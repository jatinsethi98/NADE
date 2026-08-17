//! Turning mailbox state into Gmail's exact JSON.
//!
//! The awkward parts are the point, so they are all here in one place:
//!
//! * `historyId` and `internalDate` are **strings**; `sizeEstimate`,
//!   `resultSizeEstimate` and `body.size` are **numbers**. A client that types
//!   any of them the other way round fails here.
//! * `resultSizeEstimate` is an *estimate*. The live sample in
//!   `backend/testdata/live/list.json` returned 500 messages with a
//!   `nextPageToken` and an estimate of **501**, so that is what the simulator
//!   produces: returned count, plus one when another page exists. A client that
//!   treats it as a total is wrong about Gmail and wrong here.
//! * An empty listing has **no** `messages` key at all, not `"messages": []`.
//! * `format=raw` omits `payload`; `format=full` omits `raw`. They never both
//!   appear.

use serde_json::{json, Map, Value};

use crate::{
    ids::{attachment_id, b64url, b64url_decode, b64url_nopad},
    mime::MimePart,
    StoredMessage,
};

/// The `format=` parameter of `users.messages.get`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    /// Ids, labels and metadata only — no headers, no body, no payload.
    Minimal,
    /// Adds `payload` with headers, but no `parts` and no body data.
    Metadata,
    /// The whole parsed part tree. Gmail's default.
    #[default]
    Full,
    /// The entire message, base64url, in `raw`. No `payload`.
    Raw,
}

impl Format {
    /// Parse the query parameter, case-insensitively. Unknown values fall back
    /// to `full`, which is Gmail's default.
    #[must_use]
    pub fn parse(value: Option<&str>) -> Self {
        match value.unwrap_or("full").to_ascii_lowercase().as_str() {
            "minimal" => Self::Minimal,
            "metadata" => Self::Metadata,
            "raw" => Self::Raw,
            _ => Self::Full,
        }
    }
}

/// The fields every format carries.
fn common(message: &StoredMessage) -> Map<String, Value> {
    let mut object = Map::new();
    object.insert("id".to_owned(), json!(message.id));
    object.insert("threadId".to_owned(), json!(message.thread_id));
    object.insert("labelIds".to_owned(), json!(message.label_ids));
    object.insert("snippet".to_owned(), json!(message.snippet()));
    // Strings. Both of them. Every time.
    object.insert(
        "historyId".to_owned(),
        json!(message.history_id.to_string()),
    );
    object.insert(
        "internalDate".to_owned(),
        json!(message.internal_date_ms.to_string()),
    );
    object.insert("sizeEstimate".to_owned(), json!(message.size_estimate()));
    object
}

/// Render one message in the requested format.
///
/// `metadata_headers` is the `metadataHeaders` parameter: when non-empty and the
/// format is `metadata`, only those headers come back, matched
/// case-insensitively.
#[must_use]
pub fn message_json(message: &StoredMessage, format: Format, metadata_headers: &[String]) -> Value {
    let mut object = common(message);
    match format {
        Format::Minimal => {}
        Format::Raw => {
            object.insert("raw".to_owned(), json!(b64url(&message.raw)));
        }
        Format::Metadata => {
            let tree = message.parts();
            object.insert(
                "payload".to_owned(),
                metadata_payload(&tree, metadata_headers),
            );
        }
        Format::Full => {
            let tree = message.parts();
            object.insert("payload".to_owned(), full_payload(&message.id, &tree));
        }
    }
    Value::Object(object)
}

/// `format=metadata`: the root part's headers, no `parts`, no body data.
fn metadata_payload(tree: &MimePart, wanted: &[String]) -> Value {
    let headers: Vec<Value> = tree
        .headers
        .iter()
        .filter(|(name, _)| {
            wanted.is_empty() || wanted.iter().any(|want| want.eq_ignore_ascii_case(name))
        })
        .map(|(name, value)| json!({ "name": name, "value": value }))
        .collect();
    json!({
        "partId": tree.part_id,
        "mimeType": tree.mime_type,
        "filename": tree.filename,
        "headers": headers,
        // Always zero here: metadata carries no body, and a client that reads
        // `body.size` from a metadata fetch is reading a placeholder.
        "body": { "size": 0 },
    })
}

/// `format=full`: the whole tree.
fn full_payload(message_id: &str, part: &MimePart) -> Value {
    let mut object = Map::new();
    object.insert("partId".to_owned(), json!(part.part_id));
    object.insert("mimeType".to_owned(), json!(part.mime_type));
    object.insert("filename".to_owned(), json!(part.filename));
    object.insert(
        "headers".to_owned(),
        Value::Array(
            part.headers
                .iter()
                .map(|(name, value)| json!({ "name": name, "value": value }))
                .collect(),
        ),
    );

    let mut body = Map::new();
    if part.parts.is_empty() {
        if part.is_attachment {
            // An attachment part carries an id and a size and **no data**. A
            // client that expects `data` everywhere never fetches the
            // attachment endpoint and silently loses every attachment.
            body.insert(
                "attachmentId".to_owned(),
                json!(attachment_id(message_id, &part.part_id)),
            );
            body.insert("size".to_owned(), json!(part.body.len()));
        } else {
            body.insert("size".to_owned(), json!(part.body.len()));
            if !part.body.is_empty() {
                // Transfer-decoded, then base64url. The part still advertises
                // `Content-Transfer-Encoding: base64` in `headers`, exactly as
                // Gmail leaves it.
                body.insert("data".to_owned(), json!(b64url(&part.body)));
            }
        }
    } else {
        body.insert("size".to_owned(), json!(0));
    }
    object.insert("body".to_owned(), Value::Object(body));

    if !part.parts.is_empty() {
        object.insert(
            "parts".to_owned(),
            Value::Array(
                part.parts
                    .iter()
                    .map(|child| full_payload(message_id, child))
                    .collect(),
            ),
        );
    }
    Value::Object(object)
}

/// A `users.messages.list` entry: ids and nothing else.
#[must_use]
pub fn message_reference(message: &StoredMessage) -> Value {
    json!({ "id": message.id, "threadId": message.thread_id })
}

/// The `users.messages.list` envelope.
///
/// `total_is_exact` is false whenever the caller knows another page exists.
#[must_use]
pub fn list_envelope(key: &str, entries: Vec<Value>, next_page_token: Option<&str>) -> Value {
    let mut object = Map::new();
    let count = entries.len();
    // EDGE: an empty result has no `messages` key at all. Gmail omits it, and a
    // client that indexes `body["messages"][0]` on an empty mailbox must see the
    // same `null` it would see in production.
    if count > 0 {
        object.insert(key.to_owned(), Value::Array(entries));
    }
    if let Some(token) = next_page_token {
        object.insert("nextPageToken".to_owned(), json!(token));
    }
    let estimate = count + usize::from(next_page_token.is_some());
    object.insert("resultSizeEstimate".to_owned(), json!(estimate));
    Value::Object(object)
}

/// `users.getProfile`.
#[must_use]
pub fn profile_json(
    email_address: &str,
    messages_total: usize,
    threads_total: usize,
    history_id: u64,
) -> Value {
    json!({
        "emailAddress": email_address,
        "messagesTotal": messages_total,
        "threadsTotal": threads_total,
        // A string, like everywhere else it appears.
        "historyId": history_id.to_string(),
    })
}

/// `users.threads.get`.
#[must_use]
pub fn thread_json(
    thread_id: &str,
    messages: &[&StoredMessage],
    format: Format,
    metadata_headers: &[String],
) -> Value {
    let history = messages
        .iter()
        .map(|message| message.history_id)
        .max()
        .unwrap_or_default();
    json!({
        "id": thread_id,
        "historyId": history.to_string(),
        "messages": messages
            .iter()
            .map(|message| message_json(message, format, metadata_headers))
            .collect::<Vec<_>>(),
    })
}

/// A `users.threads.list` entry: id, snippet, and the thread's `historyId`.
///
/// `messages` arrives oldest-first, the way `Mailbox::thread` returns it. The
/// snippet comes from the **newest** message, which is what a thread list shows
/// and what Gmail returns — taking the oldest would make a long thread's
/// preview permanently stale.
#[must_use]
pub fn thread_reference(thread_id: &str, messages: &[&StoredMessage]) -> Value {
    let history = messages
        .iter()
        .map(|message| message.history_id)
        .max()
        .unwrap_or_default();
    json!({
        "id": thread_id,
        "snippet": messages.last().map(|m| m.snippet()).unwrap_or_default(),
        "historyId": history.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Page tokens
// ---------------------------------------------------------------------------

/// The version byte on every page token. Bumping it invalidates old tokens,
/// which is what a real server does when its cursor format changes.
const TOKEN_VERSION: u8 = 1;

/// A keyset cursor into a listing.
///
/// **Keyset, not offset.** The cursor names the last row returned, so a page-2
/// fetch after page 1's last message was deleted still resumes in the right
/// place: it skips nothing and repeats nothing. An offset cursor would silently
/// shift the whole listing by one, and a client that reconciles by id would
/// never notice the row it never saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageCursor {
    /// A fingerprint of the query this token belongs to.
    pub scope: u64,
    /// The `internalDate` of the last row returned.
    pub after_internal_date_ms: i64,
    /// The id of the last row returned, breaking ties.
    pub after_id: String,
}

impl PageCursor {
    /// Encode as an opaque, deterministic, URL-safe token.
    #[must_use]
    pub fn encode(&self) -> String {
        b64url_nopad(
            format!(
                "{TOKEN_VERSION}\u{1}{}\u{1}{}\u{1}{}",
                self.scope, self.after_internal_date_ms, self.after_id
            )
            .as_bytes(),
        )
    }

    /// Decode a token. `None` for anything malformed — the caller turns that
    /// into a `400`, because silently restarting at page 1 would make an
    /// infinite paging loop look like success.
    #[must_use]
    pub fn decode(token: &str) -> Option<Self> {
        let raw = b64url_decode(token)?;
        let text = String::from_utf8(raw).ok()?;
        let mut fields = text.split('\u{1}');
        if fields.next()? != TOKEN_VERSION.to_string() {
            return None;
        }
        Some(Self {
            scope: fields.next()?.parse().ok()?,
            after_internal_date_ms: fields.next()?.parse().ok()?,
            after_id: fields.next()?.to_owned(),
        })
    }
}

/// A cursor into `users.history.list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryCursor {
    /// A fingerprint of the request this token belongs to.
    pub scope: u64,
    /// The id of the last record returned.
    pub after_history_id: u64,
}

impl HistoryCursor {
    /// Encode as an opaque token.
    #[must_use]
    pub fn encode(&self) -> String {
        b64url_nopad(
            format!(
                "{TOKEN_VERSION}\u{1}h\u{1}{}\u{1}{}",
                self.scope, self.after_history_id
            )
            .as_bytes(),
        )
    }

    /// Decode a token, or `None`.
    #[must_use]
    pub fn decode(token: &str) -> Option<Self> {
        let raw = b64url_decode(token)?;
        let text = String::from_utf8(raw).ok()?;
        let mut fields = text.split('\u{1}');
        if fields.next()? != TOKEN_VERSION.to_string() || fields.next()? != "h" {
            return None;
        }
        Some(Self {
            scope: fields.next()?.parse().ok()?,
            after_history_id: fields.next()?.parse().ok()?,
        })
    }
}

/// FNV-1a, 64 bit.
///
/// Hand-rolled rather than `DefaultHasher` because determinism across processes
/// and across compiler versions is a promise this crate makes, and
/// `DefaultHasher`'s output is explicitly not stable across releases.
#[must_use]
pub fn fingerprint(parts: &[&str]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for byte in part.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        // A separator, so ["ab","c"] and ["a","bc"] differ.
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MessageSpec;

    fn stored(spec: &MessageSpec) -> StoredMessage {
        StoredMessage {
            id: "1a00000000000001".to_owned(),
            thread_id: "1a00000000000001".to_owned(),
            label_ids: vec!["INBOX".to_owned(), "UNREAD".to_owned()],
            internal_date_ms: 1_785_542_400_000,
            raw: spec.to_raw_at(1, 1_785_542_400_000),
            history_id: 1_000_042,
        }
    }

    #[test]
    fn history_id_and_internal_date_are_strings_everywhere() {
        let message = stored(&MessageSpec::new().text("x"));
        for format in [Format::Minimal, Format::Metadata, Format::Full, Format::Raw] {
            let body = message_json(&message, format, &[]);
            assert!(body["historyId"].is_string(), "{format:?}");
            assert_eq!(body["historyId"], "1000042");
            assert!(body["internalDate"].is_string(), "{format:?}");
            assert_eq!(body["internalDate"], "1785542400000");
            assert!(body["sizeEstimate"].is_number(), "{format:?}");
        }
        let profile = profile_json("me@x.y", 3, 2, 1_000_042);
        assert!(profile["historyId"].is_string());
        assert!(profile["messagesTotal"].is_number());
    }

    #[test]
    fn minimal_has_no_payload_and_no_raw() {
        let body = message_json(&stored(&MessageSpec::new()), Format::Minimal, &[]);
        let object = body.as_object().unwrap();
        assert!(!object.contains_key("payload"));
        assert!(!object.contains_key("raw"));
        assert_eq!(object.len(), 7);
    }

    #[test]
    fn raw_is_the_exact_bytes_and_omits_payload() {
        let message = stored(&MessageSpec::new().text("hello"));
        let body = message_json(&message, Format::Raw, &[]);
        assert!(
            body.get("payload").is_none(),
            "raw and payload never coexist"
        );
        let decoded = b64url_decode(body["raw"].as_str().unwrap()).unwrap();
        assert_eq!(decoded, message.raw, "raw must round-trip byte for byte");
    }

    #[test]
    fn metadata_has_headers_but_no_parts_and_no_data() {
        let message = stored(&MessageSpec::new().text("a").html("<b>a</b>"));
        let body = message_json(&message, Format::Metadata, &[]);
        let payload = &body["payload"];
        assert!(payload.get("parts").is_none(), "metadata carries no parts");
        assert_eq!(payload["body"]["size"], 0);
        assert!(payload["body"].get("data").is_none());
        assert!(body.get("raw").is_none());
        let names: Vec<&str> = payload["headers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|header| header["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"Subject"));
        assert!(names.contains(&"From"));
    }

    #[test]
    fn metadata_headers_filters_case_insensitively() {
        let message = stored(&MessageSpec::new().text("a"));
        let wanted = vec!["subject".to_owned(), "DATE".to_owned()];
        let body = message_json(&message, Format::Metadata, &wanted);
        let names: Vec<String> = body["payload"]["headers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|header| header["name"].as_str().unwrap().to_ascii_lowercase())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.contains(&"subject".to_owned()));
        assert!(names.contains(&"date".to_owned()));
    }

    #[test]
    fn full_carries_the_tree_with_decoded_data() {
        let message = stored(&MessageSpec::new().text("plain").html("<b>rich</b>"));
        let body = message_json(&message, Format::Full, &[]);
        let payload = &body["payload"];
        assert_eq!(payload["partId"], "");
        assert_eq!(payload["mimeType"], "multipart/alternative");
        assert_eq!(payload["body"]["size"], 0, "a multipart node has no body");
        assert!(payload["body"].get("data").is_none());

        let parts = payload["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["partId"], "0");
        assert_eq!(parts[0]["mimeType"], "text/plain");
        assert_eq!(parts[0]["body"]["size"], 5);
        let data = b64url_decode(parts[0]["body"]["data"].as_str().unwrap()).unwrap();
        assert_eq!(data, b"plain");
        assert!(body.get("raw").is_none(), "full and raw never coexist");
    }

    #[test]
    fn an_attachment_part_has_an_id_and_no_data() {
        let message = stored(&MessageSpec::new().text("see attached").attachment(
            "r.pdf",
            "application/pdf",
            b"%PDF-1.4".to_vec(),
        ));
        let body = message_json(&message, Format::Full, &[]);
        let attachment = &body["payload"]["parts"][1];
        assert_eq!(attachment["filename"], "r.pdf");
        assert!(attachment["body"]["attachmentId"].is_string());
        assert!(
            attachment["body"].get("data").is_none(),
            "attachment bytes come from attachments.get, never inline"
        );
        assert_eq!(attachment["body"]["size"], 8);
    }

    #[test]
    fn an_empty_part_has_a_size_and_no_data_key() {
        let message = stored(&MessageSpec::new().text(""));
        let body = message_json(&message, Format::Full, &[]);
        assert_eq!(body["payload"]["body"]["size"], 0);
        assert!(body["payload"]["body"].get("data").is_none());
    }

    #[test]
    fn an_empty_listing_has_no_messages_key() {
        let body = list_envelope("messages", Vec::new(), None);
        assert!(body.get("messages").is_none(), "{body}");
        assert_eq!(body["resultSizeEstimate"], 0);
        assert!(body.get("nextPageToken").is_none());
    }

    #[test]
    fn result_size_estimate_matches_the_live_sample() {
        // backend/testdata/live/list.json: 500 rows, a nextPageToken, and an
        // estimate of 501. Not a count — an estimate.
        let rows: Vec<Value> = (0..500).map(|n| json!({ "id": n.to_string() })).collect();
        let body = list_envelope("messages", rows, Some("tok"));
        assert_eq!(body["resultSizeEstimate"], 501);
        assert_eq!(body["messages"].as_array().unwrap().len(), 500);

        // Last page: no token, and now the estimate happens to be exact.
        let rows: Vec<Value> = (0..3).map(|n| json!({ "id": n.to_string() })).collect();
        let body = list_envelope("messages", rows, None);
        assert_eq!(body["resultSizeEstimate"], 3);
    }

    #[test]
    fn page_cursors_round_trip_and_reject_junk() {
        let cursor = PageCursor {
            scope: 0xdead_beef,
            after_internal_date_ms: 1_785_542_400_000,
            after_id: "1a00000000000009".to_owned(),
        };
        let token = cursor.encode();
        assert!(!token.contains('/') && !token.contains('+'), "URL safe");
        assert_eq!(PageCursor::decode(&token), Some(cursor));

        for junk in ["", "!!!", "YQ", "%%%", "AAAAAAAA"] {
            assert_eq!(PageCursor::decode(junk), None, "{junk:?}");
        }
        // A history token is not a message token.
        let history = HistoryCursor {
            scope: 1,
            after_history_id: 2,
        }
        .encode();
        assert_eq!(PageCursor::decode(&history), None);
    }

    #[test]
    fn history_cursors_round_trip() {
        let cursor = HistoryCursor {
            scope: 7,
            after_history_id: 1_000_123,
        };
        assert_eq!(HistoryCursor::decode(&cursor.encode()), Some(cursor));
        assert_eq!(HistoryCursor::decode("nope"), None);
    }

    #[test]
    fn fingerprints_are_stable_and_separator_safe() {
        assert_eq!(fingerprint(&["a", "b"]), fingerprint(&["a", "b"]));
        assert_ne!(fingerprint(&["ab", ""]), fingerprint(&["a", "b"]));
        assert_ne!(
            fingerprint(&["newer_than:30d"]),
            fingerprint(&["is:unread"])
        );
        // Pinned, so a change to the hash is a deliberate act and not a
        // silent invalidation of every token a test has written down.
        assert_eq!(fingerprint(&["newer_than:30d"]), 0x452f_8e83_a682_2da4);
    }

    #[test]
    fn formats_parse_with_gmails_default() {
        assert_eq!(Format::parse(None), Format::Full);
        assert_eq!(Format::parse(Some("RAW")), Format::Raw);
        assert_eq!(Format::parse(Some("minimal")), Format::Minimal);
        assert_eq!(Format::parse(Some("metadata")), Format::Metadata);
        assert_eq!(Format::parse(Some("wibble")), Format::Full);
    }
}
