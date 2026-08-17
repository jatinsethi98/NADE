//! Deterministic identifiers, tokens and boundaries.
//!
//! Nothing here consults a clock or an RNG. Gmail's real ids are opaque, so the
//! simulator is free to choose them — but it is *not* free to choose them
//! randomly, because the whole crate promises byte-identical replay.
//!
//! The shapes still imitate Gmail closely enough that a client cannot get away
//! with assuming something false about them:
//!
//! * message and thread ids are 16 lowercase hex digits, like `1a002c5b7030af82`;
//! * `attachmentId` is a long opaque base64url blob with Gmail's `ANGjdJ` prefix;
//! * page tokens are opaque base64url and are **bound to the query that
//!   produced them**, so a client cannot carry one across searches.

use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD},
    Engine as _,
};

/// Base64url **with** padding — the encoding Gmail uses for `raw`,
/// `payload.body.data` and `attachments.get`'s `data`.
///
/// Google's own client samples call `base64.urlsafe_b64decode` straight on
/// these fields, which raises on missing padding, so padded is the faithful
/// choice. A client that strips padding before decoding still works; one that
/// assumes *no* padding does not, and that is a bug worth surfacing.
#[must_use]
pub fn b64url(bytes: &[u8]) -> String {
    URL_SAFE.encode(bytes)
}

/// Base64url without padding, for internal tokens where Gmail's own encoding is
/// not observable.
#[must_use]
pub fn b64url_nopad(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Decode base64url, tolerating both padded and unpadded input.
///
/// EDGE: a client that round-trips a token through a URL encoder may lose the
/// padding. Accepting both is what Gmail does and costs nothing.
#[must_use]
pub fn b64url_decode(text: &str) -> Option<Vec<u8>> {
    let trimmed = text.trim_end_matches('=');
    URL_SAFE_NO_PAD
        .decode(trimmed)
        .ok()
        .or_else(|| STANDARD.decode(text).ok())
}

/// A 16-hex-digit id in Gmail's shape, derived from a sequence number.
///
/// Gmail's real message ids are ascending-ish with time; the simulator's are
/// strictly ascending with insertion order, which keeps `(internalDate, id)`
/// ordering total and therefore keyset paging well-defined.
#[must_use]
pub fn hex16(prefix: u64, seq: u64) -> String {
    format!("{:016x}", prefix.wrapping_add(seq))
}

/// The base every simulated message id counts up from.
///
/// Chosen to sit in the same range as the live sample's real ids
/// (`1a00…`), so a test that mixes seeded and synthesised messages still sorts
/// them the way Gmail would.
pub const MESSAGE_ID_BASE: u64 = 0x1a00_0000_0000_0000;

/// Gmail's `attachmentId` is opaque but stable for a given message+part, and
/// long. The simulator makes it reversible so `attachments.get` can resolve it
/// without a second index — a real server would look it up, and the difference
/// is not observable from the wire.
#[must_use]
pub fn attachment_id(message_id: &str, part_id: &str) -> String {
    format!(
        "ANGjdJ{}",
        b64url_nopad(format!("{message_id}\u{1}{part_id}").as_bytes())
    )
}

/// Undo [`attachment_id`]. Returns `(message_id, part_id)`.
#[must_use]
pub fn parse_attachment_id(id: &str) -> Option<(String, String)> {
    let body = id.strip_prefix("ANGjdJ")?;
    let raw = b64url_decode(body)?;
    let text = String::from_utf8(raw).ok()?;
    let (message, part) = text.split_once('\u{1}')?;
    Some((message.to_owned(), part.to_owned()))
}

/// A deterministic MIME boundary for a synthesised message.
#[must_use]
pub fn mime_boundary(seq: u64, depth: u32) -> String {
    format!("----=_nade_sim_{seq:08x}_{depth}")
}

/// A deterministic `multipart/mixed` boundary for a batch **response**.
///
/// Gmail's is `batch_<random>`; the simulator's counts, so two runs of the same
/// script produce the same bytes.
#[must_use]
pub fn batch_boundary(seq: u64) -> String {
    format!("batch_nadesim_{seq:012x}")
}

/// A deterministic `Message-ID` header value.
#[must_use]
pub fn message_id_header(seq: u64, domain: &str) -> String {
    format!(
        "<{:016x}.{seq}@{domain}>",
        MESSAGE_ID_BASE.wrapping_add(seq)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_ids_look_like_gmails_and_sort_with_insertion_order() {
        let first = hex16(MESSAGE_ID_BASE, 0);
        let second = hex16(MESSAGE_ID_BASE, 1);
        assert_eq!(first.len(), 16);
        assert_eq!(first, "1a00000000000000");
        assert!(first < second, "ids must sort in insertion order");
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn attachment_ids_round_trip() {
        let id = attachment_id("1a002c5b7030af82", "1.2");
        assert!(id.starts_with("ANGjdJ"));
        assert!(id.len() > 20, "must look opaque, not like a path");
        assert_eq!(
            parse_attachment_id(&id),
            Some(("1a002c5b7030af82".to_owned(), "1.2".to_owned()))
        );
    }

    #[test]
    fn a_bogus_attachment_id_is_none_not_a_panic() {
        assert_eq!(parse_attachment_id(""), None);
        assert_eq!(parse_attachment_id("nope"), None);
        assert_eq!(parse_attachment_id("ANGjdJ!!!!"), None);
        assert_eq!(parse_attachment_id("ANGjdJaGk"), None); // decodes, but no \u{1}
    }

    #[test]
    fn base64url_is_padded_and_url_safe() {
        // 0xfb 0xff picks the two alphabet positions that differ from standard
        // base64 (`+` -> `-`, `/` -> `_`).
        let encoded = b64url(&[0xfb, 0xff, 0xfe]);
        assert_eq!(encoded, "-__-");
        let padded = b64url(b"a");
        assert!(padded.ends_with('='), "Gmail pads: {padded}");
        assert_eq!(b64url_decode(&padded).as_deref(), Some(&b"a"[..]));
    }

    #[test]
    fn decoding_tolerates_missing_padding() {
        assert_eq!(b64url_decode("YQ").as_deref(), Some(&b"a"[..]));
        assert_eq!(b64url_decode("YQ==").as_deref(), Some(&b"a"[..]));
    }
}
