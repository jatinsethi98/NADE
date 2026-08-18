//! Opaque keyset cursors.
//!
//! `API.md` §0: a cursor is base64url and **keyset**, not offset -
//! `{"ts": "…", "id": "…"}` of the last row on the page. That is what stops a
//! message arriving mid-scroll from either duplicating a row or skipping one,
//! which is exactly what `offset`/`limit` does under a `ts desc` sort.
//!
//! An unknown or corrupt cursor is `400 bad_request`, **never** a silent reset
//! to page one: silently restarting an infinite scroll looks to the user like
//! the list randomly jumped to the top.

use base64::Engine as _;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Payload {
    ts: String,
    id: String,
}

/// `GET /search` pages through **Gmail's** `pageToken`, not through a keyset of
/// ours: the rows come back in Gmail's relevance order, so there is no `(ts,
/// id)` to walk (`docs/SEARCH.md`). The token is still wrapped rather than
/// handed over raw, so the cursor stays opaque, stays one of ours, and a
/// corrupt one is still a `400` rather than a silent restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TokenPayload {
    pt: String,
}

/// The keyset position of the last row on a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keyset {
    pub ts: DateTime<Utc>,
    pub id: String,
}

/// Encode a cursor. Second-precision UTC with a `Z`, exactly like every other
/// timestamp on the wire.
#[must_use]
pub fn encode(ts: DateTime<Utc>, id: &str) -> String {
    let payload = Payload {
        ts: ts.to_rfc3339_opts(SecondsFormat::Secs, true),
        id: id.to_owned(),
    };
    // `serde_json::to_vec` on two plain strings cannot fail.
    let json = serde_json::to_vec(&payload).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

/// Decode a cursor.
///
/// # Errors
/// [`ApiError::bad_request`] for anything that is not one of ours - wrong
/// alphabet, wrong JSON, wrong keys, unparseable timestamp, or an empty id.
pub fn decode(raw: &str) -> ApiResult<Keyset> {
    let refuse = || ApiError::bad_request("That page marker is not valid. Reload the list.");

    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw.trim())
        .map_err(|_| refuse())?;
    let payload: Payload = serde_json::from_slice(&bytes).map_err(|_| refuse())?;
    if payload.id.is_empty() {
        return Err(refuse());
    }
    let ts = DateTime::parse_from_rfc3339(&payload.ts)
        .map_err(|_| refuse())?
        .with_timezone(&Utc);

    Ok(Keyset { ts, id: payload.id })
}

/// Wrap Gmail's `pageToken` in one of our cursors.
#[must_use]
pub fn encode_page_token(token: &str) -> String {
    let payload = TokenPayload {
        pt: token.to_owned(),
    };
    let json = serde_json::to_vec(&payload).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

/// Unwrap a search cursor back into Gmail's `pageToken`.
///
/// # Errors
/// [`ApiError::bad_request`] for anything that is not one of ours - including a
/// *keyset* cursor, which is a different kind of position and would page the
/// wrong list.
pub fn decode_page_token(raw: &str) -> ApiResult<String> {
    let refuse = || ApiError::bad_request("That page marker is not valid. Search again.");

    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw.trim())
        .map_err(|_| refuse())?;
    let payload: TokenPayload = serde_json::from_slice(&bytes).map_err(|_| refuse())?;
    if payload.pt.is_empty() {
        return Err(refuse());
    }
    Ok(payload.pt)
}

/// `limit`, clamped to the contract: default 50, maximum 100.
#[must_use]
pub fn clamp_limit(requested: Option<i64>, default: i64, max: i64) -> i64 {
    requested.unwrap_or(default).clamp(1, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn a_cursor_round_trips() {
        let ts = at("2026-08-16T09:12:04Z");
        let cursor = encode(ts, "18f2a1b3c4d5e6f7");
        let back = decode(&cursor).unwrap();
        assert_eq!(back.ts, ts);
        assert_eq!(back.id, "18f2a1b3c4d5e6f7");
    }

    #[test]
    fn a_cursor_is_base64url_and_opaque() {
        let cursor = encode(at("2026-08-16T09:12:04Z"), "18f2a1b3c4d5e6f7");
        assert!(
            cursor
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "{cursor} must survive a query string untouched"
        );
        assert!(!cursor.contains('='), "unpadded, so no percent-encoding");
        // Not that a client should, but the shape is the documented one.
        let json = String::from_utf8(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&cursor)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            json,
            r#"{"ts":"2026-08-16T09:12:04Z","id":"18f2a1b3c4d5e6f7"}"#
        );
    }

    /// Criterion O6 - every way a cursor can be wrong is a 400, never a reset.
    #[test]
    fn an_unknown_or_corrupt_cursor_is_a_bad_request() {
        for bad in [
            "",
            "   ",
            "not-base64!!",
            // Valid base64url, not JSON.
            &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("hello"),
            // JSON, wrong keys.
            &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"offset":50}"#),
            // Right keys, unparseable timestamp.
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(r#"{"ts":"yesterday","id":"x"}"#),
            // Right keys, empty id - would make the keyset comparison useless.
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(r#"{"ts":"2026-08-16T09:12:04Z","id":""}"#),
            // A cursor from some other system.
            "eyJvZmZzZXQiOjUwfQ",
        ] {
            let error = decode(bad).unwrap_err();
            assert_eq!(
                error.code,
                crate::error::ErrorCode::BadRequest,
                "{bad:?} should be a bad_request"
            );
        }
    }

    /// The search cursor: Gmail's `pageToken`, wrapped so it stays opaque and
    /// stays one of ours.
    #[test]
    fn a_page_token_round_trips_and_stays_opaque() {
        // A real Gmail page token: long, and full of characters a query string
        // would otherwise have to escape.
        let token = "09876543210987654321==+/abc";
        let cursor = encode_page_token(token);
        assert_eq!(decode_page_token(&cursor).unwrap(), token);
        assert!(
            cursor
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "{cursor} must survive a query string untouched"
        );
        assert!(
            !cursor.contains("0987"),
            "the token must not be legible in the cursor: {cursor}"
        );
    }

    /// Criterion O6, for the search cursor. The interesting entries are the
    /// last two: a **keyset** cursor is perfectly valid and completely wrong
    /// here, and reading it as a page token would page a different list.
    #[test]
    fn a_corrupt_or_mistyped_page_token_is_a_bad_request() {
        let keyset = encode(at("2026-08-16T09:12:04Z"), "18f2a1b3c4d5e6f7");
        for bad in [
            "",
            "   ",
            "not-base64!!",
            &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("hello"),
            // JSON, wrong keys.
            &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"offset":50}"#),
            // Right key, empty token - would restart the search at page one.
            &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"pt":""}"#),
            // A keyset cursor, which is a position in a different kind of list.
            keyset.as_str(),
        ] {
            let error = decode_page_token(bad).unwrap_err();
            assert_eq!(
                error.code,
                crate::error::ErrorCode::BadRequest,
                "{bad:?} should be a bad_request"
            );
        }

        // And the mirror: a page-token cursor is not a keyset cursor either.
        assert!(decode(&encode_page_token("tok")).is_err());
    }

    /// EDGE (unicode): a Gmail id is hex, but the decoder must not corrupt
    /// anything it is handed.
    #[test]
    fn unicode_ids_survive() {
        let cursor = encode(at("2026-08-16T09:12:04Z"), "配送のお知らせ🚀");
        assert_eq!(decode(&cursor).unwrap().id, "配送のお知らせ🚀");
    }

    /// EDGE (pagination boundary).
    #[test]
    fn limits_are_clamped_to_the_contract() {
        assert_eq!(clamp_limit(None, 50, 100), 50);
        assert_eq!(clamp_limit(Some(1), 50, 100), 1);
        assert_eq!(clamp_limit(Some(100), 50, 100), 100);
        assert_eq!(clamp_limit(Some(101), 50, 100), 100);
        assert_eq!(clamp_limit(Some(100_000), 50, 100), 100);
        // Zero and negatives would either return nothing forever or blow up the
        // SQL; both become the smallest legal page.
        assert_eq!(clamp_limit(Some(0), 50, 100), 1);
        assert_eq!(clamp_limit(Some(-5), 50, 100), 1);
    }

    /// Second precision, always `Z`: the iOS side decodes with a fixed
    /// formatter, so a fractional second would fail to parse there.
    #[test]
    fn timestamps_are_second_precision_and_z_suffixed() {
        let ts = at("2026-08-16T09:12:04.987654Z");
        let cursor = encode(ts, "x");
        let json = String::from_utf8(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&cursor)
                .unwrap(),
        )
        .unwrap();
        assert!(json.contains(r#""ts":"2026-08-16T09:12:04Z""#), "{json}");
    }
}
