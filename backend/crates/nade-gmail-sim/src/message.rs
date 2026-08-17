//! The stored message, and the compact builder that makes one.
//!
//! A message in the mailbox is its **raw RFC-822 bytes** plus the small amount
//! of metadata Gmail keeps beside them: id, thread id, label ids, internal date,
//! and the id of the last history record that touched it. Every response format
//! is derived from those bytes, so `format=raw` is always byte-identical to what
//! a test inserted and the four formats can never disagree with each other.

use std::fmt::Write as _;

use chrono::{DateTime, Utc};

use crate::{
    ids::{message_id_header, mime_boundary},
    mime::{self, MimePart},
};

/// A message as the mailbox holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    /// Gmail's message id: 16 lowercase hex digits.
    pub id: String,
    /// The thread this message belongs to.
    pub thread_id: String,
    /// In the order Gmail would return them — **not** sorted.
    ///
    /// Gmail does not document an order for `labelIds`, so the simulator keeps
    /// insertion order. A client that compares `labelIds` as an ordered list
    /// instead of a set is wrong, and this is where it finds out.
    pub label_ids: Vec<String>,
    /// Epoch milliseconds. Gmail's receipt time, not the `Date` header.
    pub internal_date_ms: i64,
    /// The exact bytes. Never normalised, never re-wrapped.
    pub raw: Vec<u8>,
    /// The id of the most recent history record that changed this message.
    pub history_id: u64,
}

impl StoredMessage {
    /// Gmail's `sizeEstimate`.
    ///
    /// Documented as an *estimate* of the message size in bytes. The simulator
    /// uses the exact raw length, which is a legal estimate; a client must not
    /// assume the two are equal, and the rustdoc on `messages.get` says so.
    #[must_use]
    pub fn size_estimate(&self) -> u64 {
        self.raw.len() as u64
    }

    /// Does the message carry this label?
    #[must_use]
    pub fn has_label(&self, label: &str) -> bool {
        self.label_ids.iter().any(|id| id == label)
    }

    /// Append a label if it is not already there. Returns whether anything
    /// changed — the mailbox uses this to decide whether history moves.
    pub fn add_label(&mut self, label: &str) -> bool {
        if self.has_label(label) {
            return false;
        }
        self.label_ids.push(label.to_owned());
        true
    }

    /// Drop a label. Returns whether anything changed.
    pub fn remove_label(&mut self, label: &str) -> bool {
        let before = self.label_ids.len();
        self.label_ids.retain(|id| id != label);
        self.label_ids.len() != before
    }

    /// The parsed part tree.
    #[must_use]
    pub fn parts(&self) -> MimePart {
        mime::parse_message(&self.raw)
    }

    /// Gmail's `snippet`.
    ///
    /// **Approximate on purpose.** Gmail's snippet algorithm is undocumented, so
    /// no simulator can reproduce it and no client may assert on its exact text.
    /// What *is* faithful, and what a client must handle, is the shape: about
    /// 200 characters of collapsed plain text with HTML entities **escaped**, so
    /// a snippet routinely contains `&#39;` and `&amp;`.
    #[must_use]
    pub fn snippet(&self) -> String {
        let tree = self.parts();
        let text = tree
            .walk()
            .into_iter()
            .find(|part| part.mime_type == "text/plain" && !part.is_attachment)
            .map(|part| String::from_utf8_lossy(&part.body).into_owned())
            .or_else(|| {
                tree.walk()
                    .into_iter()
                    .find(|part| part.mime_type == "text/html" && !part.is_attachment)
                    .map(|part| strip_tags(&String::from_utf8_lossy(&part.body)))
            })
            .unwrap_or_default();

        let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let clipped: String = collapsed.chars().take(SNIPPET_CHARS).collect();
        html_escape(&clipped)
    }
}

/// How many characters Gmail's snippet runs to, near enough.
const SNIPPET_CHARS: usize = 200;

/// The crudest possible tag stripper. Good enough for a snippet, and nowhere
/// near good enough to be mistaken for the client's own HTML→text pass.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut inside = false;
    for character in html.chars() {
        match character {
            '<' => inside = true,
            '>' => {
                inside = false;
                out.push(' ');
            }
            other if !inside => out.push(other),
            _ => {}
        }
    }
    out
}

/// Gmail escapes its snippets. `Don't` comes back as `Don&#39;t`.
fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// Where a new message's `internalDate` comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceivedAt {
    /// Whatever the simulator's clock says at insert time.
    Now,
    /// An absolute epoch-millisecond instant.
    AtMs(i64),
    /// Relative to the clock at insert time; negative is in the past.
    OffsetMs(i64),
}

/// Which thread a new message joins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadPlacement {
    /// Its own thread, whose id is the message id — Gmail's usual shape for a
    /// first message.
    NewThread,
    /// The thread that the named **message** belongs to.
    ReplyTo(String),
    /// A thread id chosen by the test.
    ThreadId(String),
}

/// One attachment to synthesise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentSpec {
    /// The `filename` parameter.
    pub filename: String,
    /// The part's `Content-Type`.
    pub mime_type: String,
    /// The bytes, before base64.
    pub bytes: Vec<u8>,
    /// `Content-ID`, for an inline image referenced by `cid:`.
    pub content_id: Option<String>,
}

/// A compact literal description of a message to insert.
///
/// ```
/// use nade_gmail_sim::MessageSpec;
///
/// let spec = MessageSpec::new()
///     .from("Ada <ada@example.com>")
///     .to("me@example.com")
///     .subject("Café — ☕")
///     .text("morning")
///     .labels(["INBOX", "UNREAD"]);
/// let raw = spec.to_raw(7);
/// // The subject is RFC 2047 encoded, exactly as a real sender would send it.
/// assert!(String::from_utf8_lossy(&raw).contains("Subject: =?UTF-8?B?"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageSpec {
    /// Force a message id instead of letting the mailbox allocate one.
    pub forced_id: Option<String>,
    /// `From`.
    pub from: String,
    /// `To`, joined with `, `.
    pub to: Vec<String>,
    /// `Cc`.
    pub cc: Vec<String>,
    /// The subject, RFC 2047 encoded on the way out when it is not ASCII.
    pub subject: String,
    /// Skip the RFC 2047 encoder and emit this subject verbatim.
    pub subject_verbatim: bool,
    /// Extra headers, appended in order.
    pub extra_headers: Vec<(String, String)>,
    /// `text/plain` body.
    pub text: Option<String>,
    /// `text/html` body.
    pub html: Option<String>,
    /// Attachments; any at all forces `multipart/mixed`.
    pub attachments: Vec<AttachmentSpec>,
    /// Bypass generation entirely and store these bytes.
    pub raw: Option<Vec<u8>>,
    /// Labels at insert time.
    pub labels: Vec<String>,
    /// Where `internalDate` comes from.
    pub received_at: ReceivedAt,
    /// Which thread to join.
    pub thread: ThreadPlacement,
    /// Omit the `Date` header entirely (corpus case 13).
    pub omit_date_header: bool,
}

impl Default for MessageSpec {
    fn default() -> Self {
        Self {
            forced_id: None,
            from: "sender@example.com".to_owned(),
            to: vec!["me@example.com".to_owned()],
            cc: Vec::new(),
            subject: "(no subject)".to_owned(),
            subject_verbatim: false,
            extra_headers: Vec::new(),
            text: Some(String::new()),
            html: None,
            attachments: Vec::new(),
            raw: None,
            labels: vec!["INBOX".to_owned(), "UNREAD".to_owned()],
            received_at: ReceivedAt::Now,
            thread: ThreadPlacement::NewThread,
            omit_date_header: false,
        }
    }
}

impl MessageSpec {
    /// An empty spec with sensible defaults: from `sender@example.com`, to
    /// `me@example.com`, labelled `INBOX` and `UNREAD`, received now.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Store exactly these bytes. Everything that would have been generated is
    /// ignored; labels, thread and date still apply.
    #[must_use]
    pub fn from_raw(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            raw: Some(bytes.into()),
            ..Self::default()
        }
    }

    /// Pin the message id — used when seeding from the live corpus, whose file
    /// names *are* the real Gmail ids.
    #[must_use]
    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.forced_id = Some(id.into());
        self
    }

    /// Set `From`.
    #[must_use]
    pub fn from(mut self, value: impl Into<String>) -> Self {
        self.from = value.into();
        self
    }

    /// Add a `To` recipient.
    #[must_use]
    pub fn to(mut self, value: impl Into<String>) -> Self {
        self.to = vec![value.into()];
        self
    }

    /// Add a `Cc` recipient.
    #[must_use]
    pub fn cc(mut self, value: impl Into<String>) -> Self {
        self.cc.push(value.into());
        self
    }

    /// Set the subject. Encoded as RFC 2047 when it is not pure ASCII.
    #[must_use]
    pub fn subject(mut self, value: impl Into<String>) -> Self {
        self.subject = value.into();
        self.subject_verbatim = false;
        self
    }

    /// Set the subject and emit it byte-for-byte, encoder bypassed.
    #[must_use]
    pub fn subject_verbatim(mut self, value: impl Into<String>) -> Self {
        self.subject = value.into();
        self.subject_verbatim = true;
        self
    }

    /// Append an arbitrary header.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_headers.push((name.into(), value.into()));
        self
    }

    /// Set the `text/plain` body.
    #[must_use]
    pub fn text(mut self, value: impl Into<String>) -> Self {
        self.text = Some(value.into());
        self
    }

    /// Set the `text/html` body. With `text` too this becomes
    /// `multipart/alternative`.
    #[must_use]
    pub fn html(mut self, value: impl Into<String>) -> Self {
        self.html = Some(value.into());
        self
    }

    /// Drop the `text/plain` part, leaving an HTML-only message — 47% of real
    /// mail in this account (`docs/PARSER.md`).
    #[must_use]
    pub fn html_only(mut self, value: impl Into<String>) -> Self {
        self.text = None;
        self.html = Some(value.into());
        self
    }

    /// Attach a file.
    #[must_use]
    pub fn attachment(
        mut self,
        filename: impl Into<String>,
        mime_type: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Self {
        self.attachments.push(AttachmentSpec {
            filename: filename.into(),
            mime_type: mime_type.into(),
            bytes: bytes.into(),
            content_id: None,
        });
        self
    }

    /// Attach an inline part with a `Content-ID`, referenced from the HTML as
    /// `cid:…`.
    #[must_use]
    pub fn inline_attachment(
        mut self,
        content_id: impl Into<String>,
        filename: impl Into<String>,
        mime_type: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Self {
        self.attachments.push(AttachmentSpec {
            filename: filename.into(),
            mime_type: mime_type.into(),
            bytes: bytes.into(),
            content_id: Some(content_id.into()),
        });
        self
    }

    /// Replace the label set.
    #[must_use]
    pub fn labels<I, S>(mut self, labels: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.labels = labels.into_iter().map(Into::into).collect();
        self
    }

    /// Set `internalDate`.
    #[must_use]
    pub fn received_at(mut self, at: ReceivedAt) -> Self {
        self.received_at = at;
        self
    }

    /// Shorthand for [`ReceivedAt::OffsetMs`] with a negative offset.
    #[must_use]
    pub fn received_days_ago(mut self, days: i64) -> Self {
        self.received_at = ReceivedAt::OffsetMs(-days * crate::clock::DAY_MS);
        self
    }

    /// Join the thread of an existing message.
    #[must_use]
    pub fn reply_to(mut self, message_id: impl Into<String>) -> Self {
        self.thread = ThreadPlacement::ReplyTo(message_id.into());
        self
    }

    /// Join a named thread.
    #[must_use]
    pub fn thread_id(mut self, thread_id: impl Into<String>) -> Self {
        self.thread = ThreadPlacement::ThreadId(thread_id.into());
        self
    }

    /// Emit no `Date` header at all.
    #[must_use]
    pub fn without_date_header(mut self) -> Self {
        self.omit_date_header = true;
        self
    }

    /// Render the RFC-822 bytes.
    ///
    /// `seq` seeds every generated identifier — MIME boundaries and the
    /// `Message-ID` — so two runs of the same script produce identical bytes.
    /// The rendered date is filled in by the mailbox, which knows the clock;
    /// see [`Self::to_raw_at`].
    #[must_use]
    pub fn to_raw(&self, seq: u64) -> Vec<u8> {
        self.to_raw_at(seq, 0)
    }

    /// Render the RFC-822 bytes with `Date:` set from `internal_date_ms`.
    #[must_use]
    pub fn to_raw_at(&self, seq: u64, internal_date_ms: i64) -> Vec<u8> {
        if let Some(raw) = &self.raw {
            return raw.clone();
        }
        let mut head = String::new();
        let _ = writeln!(head, "MIME-Version: 1.0\r");
        let _ = writeln!(head, "From: {}\r", self.from);
        if !self.to.is_empty() {
            let _ = writeln!(head, "To: {}\r", self.to.join(", "));
        }
        if !self.cc.is_empty() {
            let _ = writeln!(head, "Cc: {}\r", self.cc.join(", "));
        }
        let subject = if self.subject_verbatim {
            self.subject.clone()
        } else {
            encode_header_word(&self.subject)
        };
        let _ = writeln!(head, "Subject: {subject}\r");
        if !self.omit_date_header {
            let _ = writeln!(head, "Date: {}\r", rfc2822(internal_date_ms));
        }
        let _ = writeln!(
            head,
            "Message-ID: {}\r",
            message_id_header(seq, "sim.example.com")
        );
        for (name, value) in &self.extra_headers {
            let _ = writeln!(head, "{name}: {value}\r");
        }

        let mut out = head.into_bytes();
        self.write_body(seq, &mut out);
        out
    }

    fn write_body(&self, seq: u64, out: &mut Vec<u8>) {
        let content = self.body_layout();
        match content {
            Layout::Text => {
                let text = self.text.clone().unwrap_or_default();
                write_leaf(
                    out,
                    "text/plain; charset=\"UTF-8\"",
                    None,
                    None,
                    text.as_bytes(),
                );
            }
            Layout::Html => {
                let html = self.html.clone().unwrap_or_default();
                write_leaf(
                    out,
                    "text/html; charset=\"UTF-8\"",
                    None,
                    None,
                    html.as_bytes(),
                );
            }
            Layout::Alternative => {
                let boundary = mime_boundary(seq, 0);
                out.extend_from_slice(
                    format!("Content-Type: multipart/alternative; boundary=\"{boundary}\"\r\n\r\n")
                        .as_bytes(),
                );
                self.write_alternative(&boundary, out);
                out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
            }
            Layout::Mixed => {
                let outer = mime_boundary(seq, 0);
                out.extend_from_slice(
                    format!("Content-Type: multipart/mixed; boundary=\"{outer}\"\r\n\r\n")
                        .as_bytes(),
                );
                out.extend_from_slice(format!("--{outer}\r\n").as_bytes());
                match (self.text.is_some(), self.html.is_some()) {
                    (true, true) => {
                        let inner = mime_boundary(seq, 1);
                        out.extend_from_slice(
                            format!(
                                "Content-Type: multipart/alternative; boundary=\"{inner}\"\r\n\r\n"
                            )
                            .as_bytes(),
                        );
                        self.write_alternative(&inner, out);
                        out.extend_from_slice(format!("--{inner}--\r\n").as_bytes());
                    }
                    (false, true) => write_leaf(
                        out,
                        "text/html; charset=\"UTF-8\"",
                        None,
                        None,
                        self.html.clone().unwrap_or_default().as_bytes(),
                    ),
                    _ => write_leaf(
                        out,
                        "text/plain; charset=\"UTF-8\"",
                        None,
                        None,
                        self.text.clone().unwrap_or_default().as_bytes(),
                    ),
                }
                for attachment in &self.attachments {
                    out.extend_from_slice(format!("\r\n--{outer}\r\n").as_bytes());
                    write_leaf(
                        out,
                        &format!("{}; name=\"{}\"", attachment.mime_type, attachment.filename),
                        Some(&attachment.filename),
                        attachment.content_id.as_deref(),
                        &attachment.bytes,
                    );
                }
                out.extend_from_slice(format!("\r\n--{outer}--\r\n").as_bytes());
            }
        }
    }

    fn write_alternative(&self, boundary: &str, out: &mut Vec<u8>) {
        if let Some(text) = &self.text {
            out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            write_leaf(
                out,
                "text/plain; charset=\"UTF-8\"",
                None,
                None,
                text.as_bytes(),
            );
            out.extend_from_slice(b"\r\n");
        }
        if let Some(html) = &self.html {
            out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            write_leaf(
                out,
                "text/html; charset=\"UTF-8\"",
                None,
                None,
                html.as_bytes(),
            );
            out.extend_from_slice(b"\r\n");
        }
    }

    fn body_layout(&self) -> Layout {
        if !self.attachments.is_empty() {
            return Layout::Mixed;
        }
        match (self.text.is_some(), self.html.is_some()) {
            (true, true) => Layout::Alternative,
            (false, true) => Layout::Html,
            _ => Layout::Text,
        }
    }
}

enum Layout {
    Text,
    Html,
    Alternative,
    Mixed,
}

/// Write one leaf part: its headers, a blank line, then its encoded body.
///
/// ASCII bodies go out as `7bit` verbatim so the raw bytes stay readable in a
/// failing test's output; anything else goes out as base64, which is what a real
/// sender does and what makes the client's transfer decoding matter.
fn write_leaf(
    out: &mut Vec<u8>,
    content_type: &str,
    filename: Option<&str>,
    content_id: Option<&str>,
    body: &[u8],
) {
    out.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
    if let Some(content_id) = content_id {
        out.extend_from_slice(format!("Content-ID: <{content_id}>\r\n").as_bytes());
    }
    if let Some(filename) = filename {
        out.extend_from_slice(
            format!("Content-Disposition: attachment; filename=\"{filename}\"\r\n").as_bytes(),
        );
    }
    let ascii = body.is_ascii() && filename.is_none();
    if ascii {
        out.extend_from_slice(b"Content-Transfer-Encoding: 7bit\r\n\r\n");
        out.extend_from_slice(body);
    } else {
        out.extend_from_slice(b"Content-Transfer-Encoding: base64\r\n\r\n");
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, body);
        for chunk in encoded.as_bytes().chunks(76) {
            out.extend_from_slice(chunk);
            out.extend_from_slice(b"\r\n");
        }
    }
}

/// RFC 2047 `B` encoding, applied only when the text is not pure ASCII.
///
/// Real senders encode; a simulator that always emitted plain UTF-8 would never
/// exercise the client's decoder, and one that always encoded would never
/// exercise the plain path.
#[must_use]
pub fn encode_header_word(text: &str) -> String {
    if text.is_ascii() {
        return text.to_owned();
    }
    format!(
        "=?UTF-8?B?{}?=",
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, text.as_bytes())
    )
}

/// Format epoch milliseconds as an RFC 2822 date, always in `+0000`.
#[must_use]
pub fn rfc2822(epoch_ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(epoch_ms)
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp_nanos(0))
        .format("%a, %d %b %Y %H:%M:%S +0000")
        .to_string()
}

/// Parse an RFC 2822 `Date` header into epoch milliseconds.
///
/// Returns `None` for a missing or unparseable date, which is corpus cases 13
/// and 14 — the caller then falls back to a clock-derived instant, exactly as
/// Gmail's own `internalDate` does not depend on the header.
#[must_use]
pub fn parse_rfc2822(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc2822(value.trim())
        .ok()
        .map(|at| at.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(raw: &[u8]) -> StoredMessage {
        StoredMessage {
            id: "1a00000000000001".to_owned(),
            thread_id: "1a00000000000001".to_owned(),
            label_ids: vec!["INBOX".to_owned(), "UNREAD".to_owned()],
            internal_date_ms: 0,
            raw: raw.to_vec(),
            history_id: 1,
        }
    }

    #[test]
    fn a_plain_spec_renders_a_readable_message() {
        let raw = MessageSpec::new()
            .from("a@b.c")
            .to("me@x.y")
            .subject("Hello")
            .text("hi there")
            .to_raw_at(1, 0);
        let text = String::from_utf8(raw).unwrap();
        assert!(text.contains("From: a@b.c\r\n"));
        assert!(text.contains("Subject: Hello\r\n"));
        assert!(text.contains("Content-Type: text/plain; charset=\"UTF-8\"\r\n"));
        assert!(text.ends_with("hi there"));
    }

    #[test]
    fn a_non_ascii_subject_is_rfc2047_encoded() {
        let raw = MessageSpec::new().subject("Café ☕").to_raw(1);
        let text = String::from_utf8(raw).unwrap();
        assert!(
            text.contains("Subject: =?UTF-8?B?Q2Fmw6kg4piV?=\r\n"),
            "{text}"
        );
        assert!(text.is_ascii(), "an encoded header block must stay ASCII");
    }

    #[test]
    fn subject_verbatim_bypasses_the_encoder() {
        let raw = MessageSpec::new().subject_verbatim("Café ☕").to_raw(1);
        assert!(String::from_utf8_lossy(&raw).contains("Subject: Café ☕\r\n"));
    }

    #[test]
    fn text_and_html_become_multipart_alternative_that_reparses() {
        let raw = MessageSpec::new()
            .text("plain body")
            .html("<p>rich body</p>")
            .to_raw(3);
        let tree = mime::parse_message(&raw);
        assert_eq!(tree.mime_type, "multipart/alternative");
        assert_eq!(tree.parts.len(), 2);
        assert_eq!(tree.parts[0].body, b"plain body");
        assert_eq!(tree.parts[1].body, b"<p>rich body</p>");
    }

    #[test]
    fn attachments_become_multipart_mixed_that_reparses() {
        let raw = MessageSpec::new()
            .text("see attached")
            .html("<p>see attached</p>")
            .attachment("report.pdf", "application/pdf", b"%PDF-1.4 bytes".to_vec())
            .to_raw(4);
        let tree = mime::parse_message(&raw);
        assert_eq!(tree.mime_type, "multipart/mixed");
        assert_eq!(tree.parts.len(), 2, "alternative + attachment");
        assert_eq!(tree.parts[0].mime_type, "multipart/alternative");
        assert_eq!(tree.parts[0].parts.len(), 2);
        let attachment = &tree.parts[1];
        assert_eq!(attachment.filename, "report.pdf");
        assert!(attachment.is_attachment);
        assert_eq!(attachment.body, b"%PDF-1.4 bytes");
    }

    #[test]
    fn an_inline_attachment_keeps_its_content_id() {
        let raw = MessageSpec::new()
            .html_only("<img src=\"cid:logo\">")
            .inline_attachment(
                "logo",
                "logo.png",
                "image/png",
                vec![0x89, b'P', b'N', b'G'],
            )
            .to_raw(5);
        let tree = mime::parse_message(&raw);
        let inline = tree.walk().into_iter().find(|p| p.filename == "logo.png");
        let inline = inline.expect("the inline part must survive");
        assert_eq!(inline.header("content-id"), Some("<logo>"));
        assert_eq!(inline.body, vec![0x89, b'P', b'N', b'G']);
    }

    #[test]
    fn html_only_has_no_plain_part() {
        let raw = MessageSpec::new().html_only("<p>x</p>").to_raw(6);
        let tree = mime::parse_message(&raw);
        assert_eq!(tree.mime_type, "text/html");
        assert!(tree.parts.is_empty());
    }

    #[test]
    fn rendering_is_byte_identical_across_calls() {
        let spec = MessageSpec::new()
            .subject("Ünicode")
            .text("a")
            .html("<b>a</b>")
            .attachment("f.bin", "application/octet-stream", vec![1, 2, 3]);
        assert_eq!(spec.to_raw_at(9, 1_000), spec.to_raw_at(9, 1_000));
    }

    #[test]
    fn the_date_header_can_be_omitted() {
        let raw = MessageSpec::new().without_date_header().to_raw_at(1, 5_000);
        assert!(!String::from_utf8_lossy(&raw).contains("Date:"));
    }

    #[test]
    fn dates_round_trip_through_rfc2822() {
        let millis = 1_785_542_400_000;
        let text = rfc2822(millis);
        assert_eq!(text, "Sat, 01 Aug 2026 00:00:00 +0000");
        // Seconds resolution: the header cannot carry the milliseconds back.
        assert_eq!(parse_rfc2822(&text), Some(millis));
    }

    #[test]
    fn an_unparseable_date_is_none_not_a_panic() {
        assert_eq!(parse_rfc2822("not a date"), None);
        assert_eq!(parse_rfc2822(""), None);
        assert_eq!(parse_rfc2822("Thu, 32 Foo 20xx"), None);
    }

    #[test]
    fn labels_are_a_set_but_keep_insertion_order() {
        let mut message = stored(b"Subject: x\r\n\r\nbody");
        assert!(!message.add_label("INBOX"), "already present");
        assert!(message.add_label("STARRED"));
        assert_eq!(message.label_ids, ["INBOX", "UNREAD", "STARRED"]);
        assert!(message.remove_label("UNREAD"));
        assert!(!message.remove_label("UNREAD"), "second removal is a no-op");
        assert_eq!(message.label_ids, ["INBOX", "STARRED"]);
    }

    #[test]
    fn snippets_are_html_escaped_like_gmails() {
        let raw = MessageSpec::new()
            .text("Don't  \n  miss  <this> & that")
            .to_raw(1);
        let snippet = stored(&raw).snippet();
        assert_eq!(snippet, "Don&#39;t miss &lt;this&gt; &amp; that");
    }

    #[test]
    fn an_html_only_message_still_has_a_snippet() {
        let raw = MessageSpec::new()
            .html_only("<style>p{}</style><p>Hello</p><p>World</p>")
            .to_raw(1);
        let snippet = stored(&raw).snippet();
        assert!(snippet.contains("Hello"), "{snippet}");
        assert!(!snippet.contains('<'), "tags must be gone: {snippet}");
    }

    #[test]
    fn a_snippet_is_clipped_to_about_two_hundred_characters() {
        let raw = MessageSpec::new().text("x ".repeat(500)).to_raw(1);
        let snippet = stored(&raw).snippet();
        assert!(
            snippet.chars().count() <= SNIPPET_CHARS + 8,
            "{}",
            snippet.len()
        );
    }

    #[test]
    fn an_empty_body_yields_an_empty_snippet_not_a_panic() {
        let raw = MessageSpec::new().text("").to_raw(1);
        assert_eq!(stored(&raw).snippet(), "");
    }

    #[test]
    fn size_estimate_is_the_raw_length() {
        let message = stored(b"Subject: x\r\n\r\nbody");
        assert_eq!(message.size_estimate(), 18);
    }
}
