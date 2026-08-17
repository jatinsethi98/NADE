//! Raw RFC-822 bytes → the fields `API.md` §2 promises.
//!
//! `docs/PARSER.md` is the specification, and it records three traps that were
//! each found by measurement rather than by reading an RFC. They are marked
//! `TRAP 1/2/3` below.
//!
//! The one hard rule: **this never panics.** A message we cannot understand
//! returns `Err`, and the sync layer writes a metadata-only row plus an
//! `audit_log` entry and carries on (PLAN.md §Gmail sync 2).

use std::{borrow::Cow, collections::HashMap};

use base64::Engine as _;
use chrono::{DateTime, TimeZone as _, Utc};
use mail_parser::{Address, Message, MessageParser, MimeHeaders as _, PartType};
use sha2::{Digest as _, Sha256};

use super::html;

/// Everything ingest needs out of one message.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedMessage {
    /// `""` when the mail has no subject - `API.md` §2 never uses null here.
    pub subject: String,
    /// `""` when the sender had no display name. We never invent one from the
    /// local part; the UI decides what to fall back to.
    pub from_name: String,
    pub from_email: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    /// `None` when the `Date` header is missing or unparseable. The caller falls
    /// back to Gmail's `internalDate`, which the sync layer always has.
    pub date: Option<DateTime<Utc>>,
    /// Never null. Empty is legal (conformance case 19).
    pub body_text: String,
    /// `None` when there is **no genuine `text/html` part** - TRAP 1.
    pub body_html: Option<String>,
    pub attachments: Vec<ParsedAttachment>,
}

/// Attachment *metadata*. The bytes are never stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAttachment {
    /// Stable, opaque, URL-safe. Assigned here rather than by Gmail: the sync
    /// fetches `format=raw`, which carries no `attachmentId` anywhere.
    /// backend/DECISIONS.md D14.
    pub att_id: String,
    pub name: String,
    pub mime: String,
    pub size_bytes: i64,
    /// The `Content-ID` header with its angle brackets removed.
    pub content_id: Option<String>,
    /// True when the HTML references this part with `cid:` (`API.md` §2).
    pub inline: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("the message is not RFC-822 at all")]
    Unparseable,
}

/// Longest subject we keep. Header-injection payloads and runaway senders both
/// stop here; the wire contract has no length for it, so the cap is ours and is
/// stated rather than silent.
const MAX_SUBJECT: usize = 4_000;

/// `API.md` §2: the list snippet is "≤ 200 chars, whitespace-collapsed".
pub const SNIPPET_CHARS: usize = 200;

/// Parse one message.
///
/// `gmail_id` only ever appears inside generated attachment ids and the rewritten
/// `cid:` URLs, so this function is pure with respect to the database.
///
/// # Errors
/// [`ParseError::Unparseable`] when the bytes are not a message at all.
pub fn parse(raw: &[u8], gmail_id: &str) -> Result<ParsedMessage, ParseError> {
    // TRAP 2 - 8-bit bytes in headers are destroyed before you can see them.
    let sanitised = sanitize_headers(raw);
    let message = MessageParser::default()
        .parse(sanitised.as_ref())
        .ok_or(ParseError::Unparseable)?;

    let (from_name, from_email) = sender(&message);
    let body_html_raw = genuine_html(&message);
    let attachments = attachments(&message, gmail_id, body_html_raw.as_deref());

    let body_html = body_html_raw.map(|html| {
        let by_content_id: HashMap<String, String> = attachments
            .iter()
            .filter_map(|a| {
                a.content_id
                    .as_ref()
                    .map(|cid| (html::normalise_content_id(cid), a.att_id.clone()))
            })
            .collect();
        html::rewrite_cid_urls(&html, gmail_id, &by_content_id)
    });

    let body_text = body_text(&message, body_html.as_deref());

    Ok(ParsedMessage {
        subject: subject(&message),
        from_name,
        from_email,
        to: address_list(message.to()),
        cc: address_list(message.cc()),
        date: date(&message),
        body_text,
        body_html,
        attachments,
    })
}

/// The `API.md` §2 snippet: whitespace-collapsed, ≤ 200 **characters** (never
/// bytes - conformance case 21 is an astral codepoint that a byte slice halves).
#[must_use]
pub fn snippet(body_text: &str) -> String {
    let collapsed = body_text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= SNIPPET_CHARS {
        return collapsed;
    }
    collapsed.chars().take(SNIPPET_CHARS).collect()
}

// -------------------------------------------------------------- headers --

/// TRAP 2 - transcode the **header block only**, and only when it is not valid
/// UTF-8.
///
/// `mail-parser` reads the header block as UTF-8 and replaces every invalid byte
/// with `U+FFFD`, so the original byte is gone and nothing downstream can
/// recover it: `Don't miss out` arrives as `Don<FFFD>t miss out`.
///
/// * **windows-1252, not latin-1** - senders that declare `iso-8859-1`
///   overwhelmingly mean cp1252, and 11 of 60 real messages here declare it.
/// * **header block only** - parts carry their own charsets and transfer
///   encodings; transcoding the body would corrupt base64 and quoted-printable.
/// * **only when invalid** - valid UTF-8 always wins, which is what keeps
///   raw-UTF-8 headers (also common) working.
#[must_use]
pub fn sanitize_headers(raw: &[u8]) -> Cow<'_, [u8]> {
    let split = find_header_end(raw);
    let (head, body) = raw.split_at(split);
    if std::str::from_utf8(head).is_ok() {
        // The common case: nothing to do, and no copy.
        return Cow::Borrowed(raw);
    }
    let (decoded, _, _) = encoding_rs::WINDOWS_1252.decode(head);
    let mut out = Vec::with_capacity(decoded.len() + body.len());
    out.extend_from_slice(decoded.as_bytes());
    out.extend_from_slice(body);
    Cow::Owned(out)
}

/// The first `\r\n\r\n` **or** `\n\n`, whichever comes first.
///
/// Gmail's `format=raw` is not guaranteed pure CRLF (conformance case 25), and a
/// CRLF-only split loses the header/body boundary entirely - which would then
/// transcode the *body* as cp1252 and corrupt every base64 part.
fn find_header_end(raw: &[u8]) -> usize {
    let crlf = find_subslice(raw, b"\r\n\r\n");
    let lf = find_subslice(raw, b"\n\n");
    match (crlf, lf) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        // No blank line at all: the whole thing is headers.
        (None, None) => raw.len(),
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// TRAP 3 - `msg.subject()` returns the **last** `Subject` when a message
/// carries more than one. Every mail client shows the first, and a second
/// `Subject` is a header-injection trick to make two clients disagree about what
/// the user is reading.
fn subject(message: &Message<'_>) -> String {
    let raw = message
        .headers()
        .iter()
        .find(|header| header.name().eq_ignore_ascii_case("subject"))
        .and_then(|header| header.value().as_text())
        .unwrap_or_default();

    let cleaned: String = raw
        .chars()
        // EDGE (unicode): control characters in a subject are a spoofing tool
        // (RTL overrides aside, a bare \n splits the row in the list).
        .filter(|c| !c.is_control())
        .collect();
    let cleaned = cleaned.trim();

    if cleaned.chars().count() > MAX_SUBJECT {
        return cleaned.chars().take(MAX_SUBJECT).collect();
    }
    cleaned.to_owned()
}

fn sender(message: &Message<'_>) -> (String, String) {
    let Some(address) = message.from().and_then(Address::first) else {
        return (String::new(), String::new());
    };
    let name = address
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or_default();
    let email = address
        .address
        .as_deref()
        .map(str::trim)
        .unwrap_or_default();
    (clean_header_text(name), clean_header_text(email))
}

/// Flatten an address header to plain addresses.
///
/// Group syntax with no members and a present-but-empty `Cc` both yield an empty
/// list, never a phantom recipient (conformance case 16).
fn address_list(address: Option<&Address<'_>>) -> Vec<String> {
    let Some(address) = address else {
        return Vec::new();
    };
    address
        .iter()
        .filter_map(|addr| addr.address.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(clean_header_text)
        .collect()
}

fn clean_header_text(value: &str) -> String {
    value.chars().filter(|c| !c.is_control()).collect()
}

/// `None` when the `Date` header is absent or nonsense (cases 13 and 14).
///
/// EDGE (clock skew): this is the *sender's* clock and is routinely wrong. The
/// sync layer prefers Gmail's `internalDate` and only uses this as a fallback.
fn date(message: &Message<'_>) -> Option<DateTime<Utc>> {
    let parsed = message.date()?;
    if !parsed.is_valid() {
        return None;
    }
    match Utc.timestamp_opt(parsed.to_timestamp(), 0) {
        chrono::LocalResult::Single(value) => Some(value),
        _ => None,
    }
}

// ----------------------------------------------------------------- body --

/// TRAP 1 - `msg.body_html(0)` **synthesises HTML from the plain-text part**
/// when no `text/html` part exists. Used naively, `body_html` is non-null for
/// every message and the iOS "View original" affordance appears on plain-text
/// mail.
fn genuine_html(message: &Message<'_>) -> Option<String> {
    let has_real_html = message
        .html_body
        .iter()
        .filter_map(|id| message.part(*id))
        .any(|part| {
            part.content_type()
                .and_then(mail_parser::ContentType::subtype)
                .is_some_and(|subtype| subtype.eq_ignore_ascii_case("html"))
        });
    if !has_real_html {
        return None;
    }
    message.body_html(0).map(std::borrow::Cow::into_owned)
}

/// `body_text` is never null.
///
/// A genuine `text/plain` part wins. Otherwise we run our own extractor over the
/// HTML - the mirror of TRAP 1, `body_text(0)`, synthesises text from HTML too,
/// but its output fuses words across block boundaries (`Claude.aiClick`), which
/// poisons both the tsvector and every agent's token budget.
fn body_text(message: &Message<'_>, body_html: Option<&str>) -> String {
    let attachment_ids: Vec<u32> = message.attachments.clone();

    let plain = message
        .text_body
        .iter()
        .filter(|id| !attachment_ids.contains(id))
        .filter_map(|id| message.part(*id))
        .find(|part| {
            // A part with no Content-Type defaults to text/plain.
            let is_plain = part
                .content_type()
                .and_then(mail_parser::ContentType::subtype)
                .is_none_or(|subtype| !subtype.eq_ignore_ascii_case("html"));
            // A `text/plain` part with `Content-Disposition: attachment` is an
            // ATTACHMENT, not the body (conformance case 18). Getting this wrong
            // appends attachment text to every body.
            let is_attachment = part
                .content_disposition()
                .is_some_and(|disposition| disposition.ctype().eq_ignore_ascii_case("attachment"));
            is_plain && !is_attachment && matches!(part.body, PartType::Text(_))
        })
        .and_then(mail_parser::MessagePart::text_contents);

    if let Some(text) = plain {
        let normalised = normalise_plain(text);
        if !normalised.is_empty() {
            return normalised;
        }
    }

    body_html.map(html::to_text).unwrap_or_default()
}

/// Plain-text bodies keep their own line breaks; only the invisible padding and
/// trailing whitespace go.
fn normalise_plain(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in text.replace("\r\n", "\n").replace('\r', "\n").split('\n') {
        let cleaned: String = line
            .chars()
            .map(|c| match c {
                '\u{00A0}' | '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' | '\u{034F}' => ' ',
                other => other,
            })
            .collect();
        lines.push(cleaned.trim_end().to_owned());
    }
    while lines.first().is_some_and(String::is_empty) {
        lines.remove(0);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

// ---------------------------------------------------------- attachments --

fn attachments(
    message: &Message<'_>,
    gmail_id: &str,
    body_html: Option<&str>,
) -> Vec<ParsedAttachment> {
    let referenced = referenced_content_ids(body_html);

    message
        .attachments
        .iter()
        .enumerate()
        .filter_map(|(index, id)| {
            let part = message.part(*id)?;
            // A nested message/rfc822 is a part, not a file the user downloads.
            if part.is_multipart() {
                return None;
            }
            let content_id = part
                .content_id()
                .map(|raw| raw.trim().trim_start_matches('<').trim_end_matches('>'))
                .filter(|value| !value.is_empty())
                .map(str::to_owned);

            let mime = part.content_type().map_or_else(
                || "application/octet-stream".to_owned(),
                |ct| match ct.subtype() {
                    Some(subtype) => format!("{}/{}", ct.ctype(), subtype).to_lowercase(),
                    None => ct.ctype().to_lowercase(),
                },
            );

            let name = part
                .attachment_name()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(clean_header_text)
                .unwrap_or_else(|| default_name(&mime, index));

            let inline = content_id
                .as_deref()
                .map(html::normalise_content_id)
                .is_some_and(|cid| referenced.contains(&cid));

            Some(ParsedAttachment {
                att_id: attachment_id(gmail_id, index, &name),
                name,
                mime,
                size_bytes: i64::try_from(part.len()).unwrap_or(i64::MAX),
                content_id,
                inline,
            })
        })
        .collect()
}

/// A part with no filename still needs one - the download would otherwise be
/// called after the endpoint.
fn default_name(mime: &str, index: usize) -> String {
    let extension = match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        "text/html" => "html",
        "text/calendar" => "ics",
        _ => "bin",
    };
    format!("attachment-{}.{extension}", index + 1)
}

/// Opaque, URL-safe, and stable across re-syncs of the same message, because it
/// is a pure function of bytes that never change.
fn attachment_id(gmail_id: &str, index: usize, name: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(gmail_id.as_bytes());
    hasher.update([0]);
    hasher.update(index.to_le_bytes());
    hasher.update([0]);
    hasher.update(name.as_bytes());
    let digest = hasher.finalize();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&digest[..16])
}

/// Every `cid:` the HTML actually points at, normalised for comparison.
fn referenced_content_ids(body_html: Option<&str>) -> Vec<String> {
    let Some(html) = body_html else {
        return Vec::new();
    };
    let lower = html.to_ascii_lowercase();
    let mut found = Vec::new();
    let mut from = 0usize;
    while let Some(at) = lower[from..].find("cid:") {
        let start = from + at + 4;
        let tail = &html[start..];
        let end = tail
            .find(['"', '\'', ')', ' ', '>', '\n', '\r', '\t'])
            .unwrap_or(tail.len());
        let reference = html::normalise_content_id(&tail[..end]);
        if !reference.is_empty() && !found.contains(&reference) {
            found.push(reference);
        }
        from = start + end.max(1);
        if from >= html.len() {
            break;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use serde_json::Value;

    use super::*;

    fn corpus_dir() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/mime"))
    }

    fn live_dir() -> PathBuf {
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../testdata/live/raw"
        ))
    }

    fn collapse(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Criterion J1 - **26/26**. A probe already reached this, so anything less
    /// is our bug. A panic is a failure, never a skip; each case's `note` says
    /// what it defends against, so it goes in the message.
    #[test]
    fn conformance_corpus() {
        let dir = corpus_dir();
        let expected: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("expected.json"))
                .expect("testdata/mime/expected.json must exist - it is the specification"),
        )
        .expect("expected.json is not valid JSON");
        let cases = expected.as_object().expect("expected.json is an object");

        let mut failures: Vec<String> = Vec::new();
        for (file, want) in cases {
            let note = want["note"].as_str().unwrap_or("(no note)");
            let raw = std::fs::read(dir.join(file))
                .unwrap_or_else(|error| panic!("reading {file}: {error}"));

            let got = match parse(&raw, "18f2a1b3c4d5e6f7") {
                Ok(parsed) => parsed,
                Err(error) => {
                    failures.push(format!("{file}: parse failed ({error}) - {note}"));
                    continue;
                }
            };

            let mut wrong = |what: String| failures.push(format!("{file}: {what}\n    note: {note}"));

            if got.subject != want["subject"].as_str().unwrap_or_default() {
                wrong(format!(
                    "subject: want {:?}, got {:?}",
                    want["subject"], got.subject
                ));
            }
            if got.from_name != want["from_name"].as_str().unwrap_or_default() {
                wrong(format!(
                    "from_name: want {:?}, got {:?}",
                    want["from_name"], got.from_name
                ));
            }
            if got.from_email != want["from_email"].as_str().unwrap_or_default() {
                wrong(format!(
                    "from_email: want {:?}, got {:?}",
                    want["from_email"], got.from_email
                ));
            }
            for (field, actual) in [("to", &got.to), ("cc", &got.cc)] {
                let want_list: Vec<String> = want[field]
                    .as_array()
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                if *actual != want_list {
                    wrong(format!("{field}: want {want_list:?}, got {actual:?}"));
                }
            }

            let want_date = want["date_utc"].as_str();
            let got_date = got.date.map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string());
            if got_date.as_deref() != want_date {
                wrong(format!("date: want {want_date:?}, got {got_date:?}"));
            }

            let haystack = collapse(&got.body_text);
            for needle in want["body_text_contains"].as_array().unwrap_or(&Vec::new()) {
                let needle = collapse(needle.as_str().unwrap_or_default());
                if !haystack.contains(&needle) {
                    wrong(format!(
                        "body_text is missing {needle:?}\n    body_text: {:?}",
                        truncate(&haystack)
                    ));
                }
            }
            for needle in want["body_text_excludes"].as_array().unwrap_or(&Vec::new()) {
                let needle = needle.as_str().unwrap_or_default();
                if got.body_text.contains(needle) {
                    wrong(format!(
                        "body_text must not contain {needle:?}\n    body_text: {:?}",
                        truncate(&haystack)
                    ));
                }
            }
            if want["body_text_is_empty"].as_bool() == Some(true) && !got.body_text.is_empty() {
                wrong(format!("body_text should be empty, got {:?}", got.body_text));
            }
            if let Some(min) = want["body_text_min_length"].as_u64() {
                let actual = got.body_text.chars().count() as u64;
                if actual < min {
                    wrong(format!("body_text should be >= {min} chars, got {actual}"));
                }
            }

            let want_html = want["body_html_present"].as_bool().unwrap_or(false);
            if got.body_html.is_some() != want_html {
                wrong(format!(
                    "body_html_present: want {want_html}, got {}",
                    got.body_html.is_some()
                ));
            }

            let want_attachments = want["attachments"].as_array().cloned().unwrap_or_default();
            if got.attachments.len() != want_attachments.len() {
                wrong(format!(
                    "attachments: want {}, got {:?}",
                    want_attachments.len(),
                    got.attachments
                        .iter()
                        .map(|a| a.name.clone())
                        .collect::<Vec<_>>()
                ));
            } else {
                for (want_att, got_att) in want_attachments.iter().zip(&got.attachments) {
                    if got_att.name != want_att["name"].as_str().unwrap_or_default() {
                        wrong(format!(
                            "attachment name: want {:?}, got {:?}",
                            want_att["name"], got_att.name
                        ));
                    }
                    if got_att.mime != want_att["mime"].as_str().unwrap_or_default() {
                        wrong(format!(
                            "attachment mime: want {:?}, got {:?}",
                            want_att["mime"], got_att.mime
                        ));
                    }
                    if got_att.inline != want_att["inline"].as_bool().unwrap_or(false) {
                        wrong(format!(
                            "attachment inline: want {:?}, got {}",
                            want_att["inline"], got_att.inline
                        ));
                    }
                    if let Some(want_cid) = want_att["content_id"].as_str() {
                        if got_att.content_id.as_deref() != Some(want_cid) {
                            wrong(format!(
                                "attachment content_id: want {want_cid:?}, got {:?}",
                                got_att.content_id
                            ));
                        }
                    }
                    // The proxy URL must be reachable: an opaque, URL-safe id.
                    assert!(
                        !got_att.att_id.is_empty()
                            && got_att
                                .att_id
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                        "{file}: att_id {:?} is not URL-safe",
                        got_att.att_id
                    );
                }
            }

            // Criterion J8 - case 10's `cid:` must be gone from the stored HTML.
            if let Some(html) = &got.body_html {
                for attachment in got.attachments.iter().filter(|a| a.inline) {
                    assert!(
                        html.contains(&format!(
                            "/v1/messages/18f2a1b3c4d5e6f7/attachments/{}",
                            attachment.att_id
                        )),
                        "{file}: inline attachment {:?} was not rewritten into body_html",
                        attachment.name
                    );
                }
            }
        }

        assert!(
            failures.is_empty(),
            "{}/{} conformance cases failed:\n\n{}",
            failures.len(),
            cases.len(),
            failures.join("\n\n")
        );
        assert_eq!(cases.len(), 26, "the corpus should hold 26 cases");
        println!("parser conformance: {}/{} cases pass", cases.len(), cases.len());
    }

    fn truncate(text: &str) -> String {
        text.chars().take(300).collect()
    }

    /// Criterion J11 - real mail, as a smoke rather than as golden output.
    /// Skips cleanly when the (gitignored) sample is absent.
    #[test]
    fn live_mail_smoke() {
        let dir = live_dir();
        if !dir.is_dir() {
            println!("live mail smoke: skipped, {} is absent", dir.display());
            return;
        }
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .expect("reading the live sample")
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| path.extension().is_some_and(|e| e == "eml"))
            .collect();
        files.sort();

        if files.is_empty() {
            println!("live mail smoke: skipped, no .eml files in {}", dir.display());
            return;
        }

        let mut html_only = 0usize;
        let mut with_attachments = 0usize;
        for path in &files {
            let raw = std::fs::read(path).expect("reading a live message");
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let gmail_id = Path::new(&*name)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();

            let parsed = parse(&raw, &gmail_id)
                .unwrap_or_else(|error| panic!("{name}: real mail must parse ({error})"));

            assert!(
                !parsed.body_text.trim().is_empty(),
                "{name}: body_text must never be empty for real mail"
            );
            assert!(
                !parsed.from_email.is_empty(),
                "{name}: every real message has a sender"
            );
            assert!(
                parsed.date.is_some(),
                "{name}: every real message has a usable Date header"
            );
            assert!(
                !parsed.body_text.contains("<script"),
                "{name}: script markup leaked into body_text"
            );
            // How many messages have no `text/plain` part at all, so their
            // `body_text` came out of our own extractor. PARSER.md measured 47%.
            if parsed.body_html.is_some() && !has_plain_part(&raw) {
                html_only += 1;
            }
            with_attachments += usize::from(!parsed.attachments.is_empty());
        }

        println!(
            "live mail smoke: {}/{} messages parsed, {} html-only (body_text synthesised), \
             {} with attachments, 0 panics",
            files.len(),
            files.len(),
            html_only,
            with_attachments
        );
    }

    /// Does this message carry a real `text/plain` part? Used only by the live
    /// smoke's reporting line.
    fn has_plain_part(raw: &[u8]) -> bool {
        let sanitised = sanitize_headers(raw);
        let Some(message) = MessageParser::default().parse(sanitised.as_ref()) else {
            return false;
        };
        let attachments = message.attachments.clone();
        message
            .text_body
            .iter()
            .filter(|id| !attachments.contains(id))
            .filter_map(|id| message.part(*id))
            .any(|part| {
                part.content_type()
                    .and_then(mail_parser::ContentType::subtype)
                    .is_none_or(|s| !s.eq_ignore_ascii_case("html"))
                    && matches!(part.body, PartType::Text(_))
            })
    }

    /// Criterion J12 - total, whatever arrives.
    #[test]
    fn garbage_input_never_panics() {
        let cases: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"\r\n\r\n".to_vec(),
            b"Subject: truncated".to_vec(),
            b"\x00\x01\x02\xff\xfe\xfd".to_vec(),
            vec![0x80; 4096],
            b"From: a@b\r\nContent-Type: multipart/mixed; boundary=\"x\"\r\n\r\n--x\r\n".to_vec(),
            b"Content-Type: text/html\r\n\r\n<p>\xff\xfe unclosed".to_vec(),
        ];
        for raw in cases {
            let _ = parse(&raw, "g");
        }
    }

    /// Criterion J3 - valid UTF-8 wins, and the body is never transcoded.
    #[test]
    fn header_sanitisation_is_utf8_first_and_header_only() {
        // Raw UTF-8 headers are left exactly alone (no copy).
        let utf8 = "Subject: Reunión mañana\r\n\r\nbody\r\n".as_bytes();
        assert!(matches!(sanitize_headers(utf8), Cow::Borrowed(_)));

        // A cp1252 byte in the header block is recovered...
        let mixed = b"Subject: Don\x92t miss out\r\n\r\n\x92\x93 raw body bytes";
        let fixed = sanitize_headers(mixed);
        let text = String::from_utf8_lossy(&fixed);
        assert!(text.contains("Don\u{2019}t miss out"), "{text}");

        // ...while the body keeps its original bytes, because parts carry their
        // own charset and transfer encoding.
        assert!(
            fixed.ends_with(b"\x92\x93 raw body bytes"),
            "the body must not be transcoded"
        );
    }

    /// Criterion J4 - Gmail's `format=raw` is not guaranteed pure CRLF.
    #[test]
    fn header_end_accepts_lf_only() {
        assert_eq!(find_header_end(b"a: b\n\nbody"), 4);
        assert_eq!(find_header_end(b"a: b\r\n\r\nbody"), 4);
        // Mixed: the CRLF pair comes first here, and wins.
        assert_eq!(find_header_end(b"a: b\r\nc: d\r\n\r\ne\n\nf"), 10);
        // No blank line at all: everything is headers, and nothing is corrupted.
        assert_eq!(find_header_end(b"a: b\r\nc: d"), 10);
    }

    /// Criterion J5 - TRAP 3, directly.
    #[test]
    fn the_first_subject_header_wins() {
        let raw = b"From: a@b.com\r\nSubject: First subject\r\nSubject: Second subject\r\n\r\nx\r\n";
        let parsed = parse(raw, "g").unwrap();
        assert_eq!(parsed.subject, "First subject");
    }

    /// Criterion J2 - TRAP 1, directly. `mail-parser` would synthesise HTML for
    /// the first of these.
    #[test]
    fn body_html_is_null_without_a_real_html_part() {
        let plain = b"From: a@b.com\r\nContent-Type: text/plain\r\n\r\nJust text.\r\n";
        assert_eq!(parse(plain, "g").unwrap().body_html, None);

        let html = b"From: a@b.com\r\nContent-Type: text/html\r\n\r\n<p>Real HTML.</p>\r\n";
        let parsed = parse(html, "g").unwrap();
        assert!(parsed.body_html.is_some());
        assert_eq!(parsed.body_text, "Real HTML.");
    }

    /// Criterion J9 + edge case 1.
    #[test]
    fn body_text_is_never_null_and_empty_is_legal() {
        let empty = b"From: a@b.com\r\nSubject: (no body)\r\n\r\n";
        let parsed = parse(empty, "g").unwrap();
        assert_eq!(parsed.body_text, "");
        assert_eq!(parsed.body_html, None);
    }

    /// EDGE (unicode) - a 200-character cap must count characters, not bytes.
    #[test]
    fn snippets_cap_characters_not_bytes() {
        let astral = "🚀".repeat(400);
        let cut = snippet(&astral);
        assert_eq!(cut.chars().count(), SNIPPET_CHARS);
        assert!(cut.ends_with('🚀'), "an astral codepoint was sliced in half");

        assert_eq!(snippet("  a\n\n b \t c  "), "a b c");
        assert_eq!(snippet(""), "");
    }

    /// A subject cannot smuggle a newline into the mail row.
    #[test]
    fn control_characters_are_stripped_from_headers() {
        let raw = b"From: a@b.com\r\nSubject: =?utf-8?B?QQpC?=\r\n\r\nx\r\n";
        assert_eq!(parse(raw, "g").unwrap().subject, "AB");
    }

    /// Attachment ids are stable across re-syncs and differ between messages.
    #[test]
    fn attachment_ids_are_stable_and_url_safe() {
        let a = attachment_id("18f2a1b3c4d5e6f7", 0, "report.pdf");
        let b = attachment_id("18f2a1b3c4d5e6f7", 0, "report.pdf");
        let c = attachment_id("18f2a1b3c4d5e6f8", 0, "report.pdf");
        let d = attachment_id("18f2a1b3c4d5e6f7", 1, "report.pdf");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
        assert_eq!(a.len(), 22);
        assert!(a
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'));
    }
}
