//! A small, independent MIME structure walker.
//!
//! This exists to turn the stored RFC-822 bytes into the `payload` tree that
//! `messages.get?format=full` returns. It is deliberately **not** `mail-parser`,
//! which is what NADE itself uses: if the simulator and the code under test
//! shared a MIME parser, a bug in that parser would be invisible to both and the
//! tests would agree on the wrong answer.
//!
//! It is a *structure* walker, not a mail parser. It does not decode RFC 2047,
//! does not guess charsets, and does not extract a body — because Gmail does not
//! either. `payload.headers[].value` comes back exactly as it sits in the
//! message, still `=?utf-8?B?…?=` encoded, and decoding it is the client's job.
//!
//! What it *does* do, because Gmail does: split a multipart body on its
//! boundary, assign Gmail's `partId` scheme (`""`, `"0"`, `"0.1"`, …), and
//! decode the `Content-Transfer-Encoding` so that `body.data` holds the decoded
//! bytes.

use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine as _,
};

/// One node of the `payload` tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MimePart {
    /// Gmail's `partId`. The root is `""`.
    pub part_id: String,
    /// Lowercased `type/subtype`; `text/plain` when there is no `Content-Type`.
    pub mime_type: String,
    /// From `Content-Disposition; filename=` or `Content-Type; name=`. Empty
    /// when the part is not a file.
    pub filename: String,
    /// The part's own headers, in order, values **undecoded**.
    pub headers: Vec<(String, String)>,
    /// The transfer-decoded body of a leaf part. Empty for a multipart node.
    pub body: Vec<u8>,
    /// Children, for a `multipart/*` node.
    pub parts: Vec<MimePart>,
    /// Whether Gmail would hand this part an `attachmentId` instead of inline
    /// `data`.
    pub is_attachment: bool,
}

impl MimePart {
    /// Depth-first walk, root first — the same order `partId`s were assigned.
    #[must_use]
    pub fn walk(&self) -> Vec<&Self> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect<'a>(&'a self, out: &mut Vec<&'a Self>) {
        out.push(self);
        for child in &self.parts {
            child.collect(out);
        }
    }

    /// First header with this name, matched case-insensitively.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Split a message into its header block and its body.
///
/// EDGE: `\n\n` as well as `\r\n\r\n`. Gmail's `format=raw` is not guaranteed
/// pure CRLF (`docs/PARSER.md` trap 3's neighbour), and a CRLF-only split loses
/// the boundary entirely on an LF-only message — the whole message would look
/// like one enormous header block.
#[must_use]
pub fn split_header_body(raw: &[u8]) -> (&[u8], &[u8]) {
    let crlf = find(raw, b"\r\n\r\n").map(|at| (at, at + 4));
    let lf = find(raw, b"\n\n").map(|at| (at, at + 2));
    match (crlf, lf) {
        (Some(a), Some(b)) => {
            let pick = if a.0 <= b.0 { a } else { b };
            (&raw[..pick.0], &raw[pick.1..])
        }
        (Some(a), None) => (&raw[..a.0], &raw[a.1..]),
        (None, Some(b)) => (&raw[..b.0], &raw[b.1..]),
        // EDGE: headers with no body at all (corpus case 19 is close to this).
        (None, None) => (raw, &[]),
    }
}

/// Unfold and split a header block into `(name, value)` pairs, in order.
///
/// Values keep their original bytes, decoded as UTF-8 lossily. A raw 8-bit byte
/// in a header becomes `U+FFFD` here, exactly as it does for a client that reads
/// the header block as UTF-8 — which is the trap `docs/PARSER.md` §2 is about.
/// The simulator must not repair it, or the client's own repair would never be
/// exercised. `format=raw` still carries the original bytes.
#[must_use]
pub fn parse_headers(block: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(block);
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            // A continuation line. RFC 5322 unfolding keeps one space.
            if let Some((_, value)) = headers.last_mut() {
                value.push(' ');
                value.push_str(line.trim_start());
            }
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_owned(), value.trim().to_owned()));
        }
        // EDGE: a header line with no colon is dropped, not treated as a
        // continuation. Treating it as one would let a malformed message
        // silently append junk to the previous header's value.
    }
    headers
}

/// Parse a structured header value into its main token and its parameters.
///
/// `text/plain; charset="utf-8"; name=a.txt` →
/// `("text/plain", [("charset", "utf-8"), ("name", "a.txt")])`.
#[must_use]
pub fn parse_parameterised(value: &str) -> (String, Vec<(String, String)>) {
    let mut parts = split_semicolons(value).into_iter();
    let head = parts.next().unwrap_or_default().trim().to_owned();
    let params = parts
        .filter_map(|chunk| {
            let (name, raw) = chunk.split_once('=')?;
            let name = name.trim().to_ascii_lowercase();
            let value = raw.trim().trim_matches('"').to_owned();
            Some((name, value))
        })
        .collect();
    (head, params)
}

/// Split on `;` but not inside a quoted string.
fn split_semicolons(value: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut quoted = false;
    for (index, byte) in value.bytes().enumerate() {
        match byte {
            b'"' => quoted = !quoted,
            b';' if !quoted => {
                out.push(&value[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    out.push(&value[start..]);
    out
}

/// Build the whole part tree from a complete RFC-822 message.
#[must_use]
pub fn parse_message(raw: &[u8]) -> MimePart {
    parse_part(raw, String::new(), 0)
}

/// Nesting deeper than this is treated as a leaf.
///
/// EDGE: a message that nests `multipart/*` inside itself forever — either
/// hostile or a broken generator — would otherwise recurse until the stack
/// dies. Gmail flattens deep nesting too; the exact depth is undocumented, so
/// this is a stated simplification, not a claim about Gmail.
const MAX_DEPTH: usize = 24;

fn parse_part(raw: &[u8], part_id: String, depth: usize) -> MimePart {
    let (header_block, body) = split_header_body(raw);
    let headers = parse_headers(header_block);

    let content_type = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| value.clone())
        .unwrap_or_default();
    let (mime_type, ct_params) = parse_parameterised(&content_type);
    // RFC 2045: no Content-Type means text/plain; charset=us-ascii.
    let mime_type = if mime_type.is_empty() {
        "text/plain".to_owned()
    } else {
        mime_type.to_ascii_lowercase()
    };

    let disposition = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-disposition"))
        .map(|(_, value)| value.clone())
        .unwrap_or_default();
    let (disposition_kind, cd_params) = parse_parameterised(&disposition);

    let filename = param(&cd_params, "filename")
        .or_else(|| param(&ct_params, "name"))
        .unwrap_or_default();

    let is_multipart = mime_type.starts_with("multipart/");
    let boundary = param(&ct_params, "boundary");

    let mut node = MimePart {
        part_id,
        mime_type,
        filename: filename.clone(),
        headers,
        body: Vec::new(),
        parts: Vec::new(),
        is_attachment: false,
    };

    if is_multipart && depth < MAX_DEPTH {
        if let Some(boundary) = boundary {
            node.parts = split_multipart(body, &boundary)
                .into_iter()
                .enumerate()
                .map(|(index, chunk)| {
                    let child_id = if node.part_id.is_empty() {
                        index.to_string()
                    } else {
                        format!("{}.{index}", node.part_id)
                    };
                    parse_part(chunk, child_id, depth + 1)
                })
                .collect();
            return node;
        }
        // EDGE: `multipart/*` with no boundary parameter. Malformed, and real.
        // Treat the whole body as one opaque leaf rather than dropping it.
    }

    node.body = decode_transfer_encoding(&node, body);
    let has_content_id = node.header("content-id").is_some();
    node.is_attachment = !filename.is_empty()
        || disposition_kind.eq_ignore_ascii_case("attachment")
        || (has_content_id && !node.mime_type.starts_with("text/"));
    node
}

fn param(params: &[(String, String)], name: &str) -> Option<String> {
    params
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
        .filter(|value| !value.is_empty())
}

/// Split a multipart body on `--boundary`, dropping preamble and epilogue.
fn split_multipart<'a>(body: &'a [u8], boundary: &str) -> Vec<&'a [u8]> {
    let delimiter = format!("--{boundary}");
    let mut out: Vec<&[u8]> = Vec::new();
    let mut rest = body;
    let mut seen_first = false;

    while let Some(at) = find(rest, delimiter.as_bytes()) {
        let after = &rest[at + delimiter.len()..];
        if seen_first {
            // Everything from the previous delimiter to this one is a part.
            let chunk = &rest[..at];
            out.push(trim_trailing_newline(chunk));
        }
        if after.starts_with(b"--") {
            // The closing delimiter. Anything past it is epilogue.
            return out;
        }
        seen_first = true;
        rest = strip_leading_newline(after);
    }

    if seen_first {
        out.push(trim_trailing_newline(rest));
    }
    out
}

fn strip_leading_newline(input: &[u8]) -> &[u8] {
    if let Some(rest) = input.strip_prefix(b"\r\n") {
        rest
    } else if let Some(rest) = input.strip_prefix(b"\n") {
        rest
    } else {
        input
    }
}

fn trim_trailing_newline(input: &[u8]) -> &[u8] {
    if let Some(rest) = input.strip_suffix(b"\r\n") {
        rest
    } else if let Some(rest) = input.strip_suffix(b"\n") {
        rest
    } else {
        input
    }
}

/// Decode `base64` and `quoted-printable`; pass everything else through.
///
/// Gmail returns `body.data` transfer-**decoded** while still reporting the
/// original `Content-Transfer-Encoding: base64` in `headers[]`. That mismatch
/// confuses people, and reproducing it is the point: a client that decodes
/// base64 a second time gets garbage, and should find that out here.
fn decode_transfer_encoding(node: &MimePart, body: &[u8]) -> Vec<u8> {
    match node
        .header("content-transfer-encoding")
        .unwrap_or("7bit")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "base64" => decode_base64_lenient(body),
        "quoted-printable" => decode_quoted_printable(body),
        _ => body.to_vec(),
    }
}

/// Base64 that never fails, in one pass.
///
/// EDGE: corpus case 22 is deliberately malformed base64. Gmail still returns
/// *something* for such a part, and a simulator that panicked would turn a
/// client-robustness test into a simulator crash.
///
/// The obvious lenient implementation — try to decode, shorten by one, try
/// again — is **quadratic**, and an 8 MB attachment whose last byte is wrong
/// would take minutes. This is linear instead: the strict decoder gets first
/// refusal, and the fallback drops everything outside the alphabet, trims the
/// one length remainder that no byte string can produce (`len % 4 == 1`), and
/// decodes once.
#[must_use]
pub fn decode_base64_lenient(body: &[u8]) -> Vec<u8> {
    let compact: Vec<u8> = body
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    // The overwhelmingly common case: it is simply valid.
    if let Ok(decoded) = STANDARD.decode(&compact) {
        return decoded;
    }
    let mut alphabet: Vec<u8> = compact
        .into_iter()
        .filter(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'-' | b'_'))
        .collect();
    // A remainder of 1 encodes no whole byte, so it can only be damage.
    if alphabet.len() % 4 == 1 {
        alphabet.pop();
    }
    base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(&alphabet)
        .or_else(|_| URL_SAFE_NO_PAD.decode(&alphabet))
        .unwrap_or_default()
}

/// Quoted-printable, soft line breaks and all.
#[must_use]
pub fn decode_quoted_printable(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let mut index = 0usize;
    while index < body.len() {
        let byte = body[index];
        if byte != b'=' {
            out.push(byte);
            index += 1;
            continue;
        }
        // `=\r\n` and `=\n` are soft breaks and vanish.
        if body[index..].starts_with(b"=\r\n") {
            index += 3;
            continue;
        }
        if body[index..].starts_with(b"=\n") {
            index += 2;
            continue;
        }
        if index + 2 < body.len() {
            let hex = &body[index + 1..index + 3];
            if let Some(value) = hex_pair(hex[0], hex[1]) {
                out.push(value);
                index += 3;
                continue;
            }
        }
        // EDGE: a bare `=` that is not an escape. RFC 2045 forbids it; senders
        // emit it anyway. Pass it through rather than dropping the rest.
        out.push(byte);
        index += 1;
    }
    out
}

fn hex_pair(high: u8, low: u8) -> Option<u8> {
    let high = (high as char).to_digit(16)?;
    let low = (low as char).to_digit(16)?;
    u8::try_from(high * 16 + low).ok()
}

pub(crate) fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_message_is_one_leaf_with_the_root_part_id() {
        let raw = b"From: a@b.c\r\nSubject: Hi\r\n\r\nbody text\r\n";
        let tree = parse_message(raw);
        assert_eq!(tree.part_id, "");
        assert_eq!(tree.mime_type, "text/plain");
        assert!(tree.parts.is_empty());
        assert_eq!(tree.body, b"body text\r\n");
        assert_eq!(tree.header("subject"), Some("Hi"));
    }

    #[test]
    fn lf_only_messages_split_at_the_right_place() {
        let raw = b"Subject: Hi\nFrom: a@b.c\n\nbody\n";
        let (head, body) = split_header_body(raw);
        assert_eq!(head, b"Subject: Hi\nFrom: a@b.c");
        assert_eq!(body, b"body\n");
    }

    #[test]
    fn crlf_wins_when_it_comes_first_and_lf_wins_when_it_does() {
        // A `\n\n` inside a folded header must not beat a later `\r\n\r\n`…
        let lf_first = b"A: 1\n\nbody\r\n\r\nmore";
        let (head, _) = split_header_body(lf_first);
        assert_eq!(head, b"A: 1");
        // …and the earlier separator always wins.
        let crlf_first = b"A: 1\r\n\r\nbody\n\nmore";
        let (head, body) = split_header_body(crlf_first);
        assert_eq!(head, b"A: 1");
        assert_eq!(body, b"body\n\nmore");
    }

    #[test]
    fn folded_headers_are_unfolded_into_one_value() {
        let raw = b"Subject: a very\r\n long subject\r\nTo: x@y.z\r\n\r\n";
        let tree = parse_message(raw);
        assert_eq!(tree.header("subject"), Some("a very long subject"));
        assert_eq!(tree.headers.len(), 2);
    }

    #[test]
    fn header_values_stay_rfc2047_encoded() {
        let raw = b"Subject: =?utf-8?B?SGVsbG8=?=\r\n\r\nx";
        let tree = parse_message(raw);
        assert_eq!(
            tree.header("subject"),
            Some("=?utf-8?B?SGVsbG8=?="),
            "decoding here would hide the client's own decoder"
        );
    }

    #[test]
    fn duplicate_headers_are_all_kept_in_order() {
        let raw = b"Subject: first\r\nSubject: second\r\n\r\nx";
        let tree = parse_message(raw);
        assert_eq!(tree.headers.len(), 2);
        assert_eq!(tree.headers[0].1, "first");
        assert_eq!(tree.headers[1].1, "second");
        // `header()` takes the first, which is what a mail client shows.
        assert_eq!(tree.header("subject"), Some("first"));
    }

    #[test]
    fn multipart_children_get_gmails_part_ids() {
        let raw = b"Content-Type: multipart/alternative; boundary=\"BB\"\r\n\r\n\
                    --BB\r\nContent-Type: text/plain\r\n\r\nplain\r\n\
                    --BB\r\nContent-Type: text/html\r\n\r\n<p>rich</p>\r\n\
                    --BB--\r\n";
        let tree = parse_message(raw);
        assert_eq!(tree.mime_type, "multipart/alternative");
        assert_eq!(tree.parts.len(), 2);
        assert_eq!(tree.parts[0].part_id, "0");
        assert_eq!(tree.parts[1].part_id, "1");
        assert_eq!(tree.parts[0].body, b"plain");
        assert_eq!(tree.parts[1].body, b"<p>rich</p>");
        assert!(
            tree.body.is_empty(),
            "a multipart node has no body of its own"
        );
    }

    #[test]
    fn nested_multipart_ids_are_dotted() {
        let raw = b"Content-Type: multipart/mixed; boundary=OUT\r\n\r\n\
                    --OUT\r\nContent-Type: multipart/alternative; boundary=IN\r\n\r\n\
                    --IN\r\nContent-Type: text/plain\r\n\r\nt\r\n\
                    --IN\r\nContent-Type: text/html\r\n\r\nh\r\n\
                    --IN--\r\n\
                    --OUT\r\nContent-Type: application/pdf\r\n\
                    Content-Disposition: attachment; filename=\"r.pdf\"\r\n\r\nPDF\r\n\
                    --OUT--\r\n";
        let tree = parse_message(raw);
        let ids: Vec<&str> = tree.walk().iter().map(|p| p.part_id.as_str()).collect();
        assert_eq!(ids, ["", "0", "0.0", "0.1", "1"]);
        let pdf = tree.walk()[4];
        assert_eq!(pdf.filename, "r.pdf");
        assert!(pdf.is_attachment);
    }

    #[test]
    fn preamble_and_epilogue_are_discarded() {
        let raw = b"Content-Type: multipart/mixed; boundary=B\r\n\r\n\
                    This is a MIME message.\r\n\
                    --B\r\nContent-Type: text/plain\r\n\r\nreal\r\n\
                    --B--\r\n\
                    trailing junk\r\n";
        let tree = parse_message(raw);
        assert_eq!(tree.parts.len(), 1);
        assert_eq!(tree.parts[0].body, b"real");
    }

    #[test]
    fn base64_and_quoted_printable_bodies_are_decoded() {
        let raw = b"Content-Type: text/plain\r\nContent-Transfer-Encoding: base64\r\n\r\n\
                    SGVsbG8sIHdvcmxkIQ==\r\n";
        assert_eq!(parse_message(raw).body, b"Hello, world!");

        let raw = b"Content-Transfer-Encoding: quoted-printable\r\n\r\n\
                    Caf=C3=A9 =\r\nbar=";
        assert_eq!(parse_message(raw).body, "Café bar=".as_bytes());
    }

    #[test]
    fn malformed_base64_decodes_as_far_as_it_can_and_never_panics() {
        let raw = b"Content-Transfer-Encoding: base64\r\n\r\nSGVsbG8h!!!!\r\n";
        let decoded = parse_message(raw).body;
        assert!(
            decoded.starts_with(b"Hell"),
            "expected a best-effort prefix, got {decoded:?}"
        );
    }

    /// EDGE: a large body whose base64 is damaged. The naive lenient decoder
    /// was quadratic here and took minutes; this must be instant.
    #[test]
    fn malformed_base64_at_scale_is_linear_not_quadratic() {
        let mut payload = b"A".repeat(2 * 1024 * 1024);
        payload.extend_from_slice(b"!!!");
        let raw = [
            b"Content-Transfer-Encoding: base64\r\n\r\n".to_vec(),
            payload,
        ]
        .concat();
        let started = std::time::Instant::now();
        let decoded = parse_message(&raw).body;
        assert!(!decoded.is_empty(), "it must still recover the good prefix");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "decoding took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn multipart_without_a_boundary_is_a_leaf_not_a_loss() {
        let raw = b"Content-Type: multipart/mixed\r\n\r\nstill here\r\n";
        let tree = parse_message(raw);
        assert!(tree.parts.is_empty());
        assert_eq!(tree.body, b"still here\r\n");
    }

    #[test]
    fn unbounded_nesting_stops_instead_of_blowing_the_stack() {
        // Every level reuses one boundary, so each part is itself a multipart.
        let mut raw = String::from("Content-Type: multipart/mixed; boundary=B\r\n\r\n");
        for _ in 0..200 {
            raw.push_str("--B\r\nContent-Type: multipart/mixed; boundary=B\r\n\r\n");
        }
        raw.push_str("--B--\r\n");
        let tree = parse_message(raw.as_bytes());
        assert!(tree.walk().len() < 10_000, "the walk must terminate");
    }

    #[test]
    fn parameters_survive_quoting_and_semicolons() {
        let (head, params) =
            parse_parameterised("application/pdf; name=\"a;b.pdf\"; charset=utf-8");
        assert_eq!(head, "application/pdf");
        assert_eq!(param(&params, "name").as_deref(), Some("a;b.pdf"));
        assert_eq!(param(&params, "charset").as_deref(), Some("utf-8"));
    }

    #[test]
    fn a_header_line_with_no_colon_is_dropped() {
        let raw = b"Subject: ok\r\ngarbage line\r\nTo: x@y\r\n\r\nbody";
        let tree = parse_message(raw);
        assert_eq!(tree.headers.len(), 2);
        assert_eq!(tree.header("subject"), Some("ok"));
        assert_eq!(tree.header("to"), Some("x@y"));
    }

    #[test]
    fn a_message_with_no_body_separator_is_all_headers() {
        let (head, body) = split_header_body(b"Subject: only\r\n");
        assert_eq!(head, b"Subject: only\r\n");
        assert!(body.is_empty());
    }

    #[test]
    fn eight_bit_header_bytes_become_replacement_chars_not_a_panic() {
        // `docs/PARSER.md` trap 2: this is the lossy read a client does, and the
        // simulator must reproduce it rather than repair it.
        let raw = b"Subject: Don\x92t miss out\r\n\r\nbody";
        let tree = parse_message(raw);
        assert!(tree.header("subject").unwrap().contains('\u{fffd}'));
    }
}
