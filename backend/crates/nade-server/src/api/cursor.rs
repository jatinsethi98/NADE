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
///
/// `qf` fingerprints the query the token was minted for. Gmail's page token is
/// only meaningful against the query that produced it, so pairing it with a
/// different `q` pages a list that no longer exists - Gmail either errors or,
/// worse, answers with results from a different search. `API.md` §0 requires
/// the 400.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TokenPayload {
    pt: String,
    qf: String,
}

/// The keyset position of the last row on a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keyset {
    pub ts: DateTime<Utc>,
    pub id: String,
}

/// Encode a cursor: UTC with a `Z`, and **as precise as the value it carries**.
///
/// `AutoSi` and not `Secs`, which is not cosmetic. A cursor is one half of a
/// keyset comparison — `(ts, id) < (cursor.ts, cursor.id)` — so it has to round
/// trip the ordering key exactly. Rows stamped by `now()` carry microseconds,
/// and a second-precision cursor silently *skipped* every row inside the
/// cursor's own second: page two of `/runs` came back empty with 51 rows in the
/// table. `AutoSi` emits no fractional part at all when there is none, so a
/// whole-second timestamp still encodes byte-identically to before and the
/// contract fixtures are unaffected.
#[must_use]
pub fn encode(ts: DateTime<Utc>, id: &str) -> String {
    let payload = Payload {
        ts: ts.to_rfc3339_opts(SecondsFormat::AutoSi, true),
        id: id.to_owned(),
    };
    // `serde_json::to_vec` on two plain strings cannot fail.
    let json = serde_json::to_vec(&payload).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

impl Keyset {
    /// The cursor's id as a UUID.
    ///
    /// Every P4 list is keyed on a UUID, but `Payload.id` is a plain string
    /// because `/mailboxes/{id}/threads` puts a **Gmail thread id** in the same
    /// field. So a perfectly well-formed cursor from another endpoint decodes
    /// here without error, and `parse().unwrap_or(Uuid::nil())` — the obvious
    /// spelling — turns it into the *smallest* UUID, which makes
    /// `(ts, id) < (ts, nil)` skip every row inside the cursor's own second.
    ///
    /// That is a silent wrong answer, and `API.md` §0 forbids even the benign
    /// version of it: "An unknown or corrupt cursor is `400 bad_request`, never
    /// a silent reset to page one."
    ///
    /// # Errors
    /// [`ApiError::bad_request`] when the id is not a UUID.
    pub fn uuid(&self) -> ApiResult<uuid::Uuid> {
        uuid::Uuid::parse_str(&self.id)
            .map_err(|_| ApiError::bad_request("That page marker is not valid. Reload the list."))
    }
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

/// Wrap Gmail's `pageToken` in one of our cursors, bound to the **canonical**
/// query it pages through - `NormalisedQuery::as_str`, never the raw `q`, so
/// two spellings of one search share a cursor instead of fighting over it.
#[must_use]
pub fn encode_page_token(token: &str, canonical_query: &str) -> String {
    let payload = TokenPayload {
        pt: token.to_owned(),
        qf: query_fingerprint(canonical_query),
    };
    let json = serde_json::to_vec(&payload).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

/// Unwrap a search cursor back into Gmail's `pageToken`, refusing one minted
/// for a different query.
///
/// # Errors
/// [`ApiError::bad_request`] for anything that is not one of ours - including a
/// *keyset* cursor, which is a different kind of position and would page the
/// wrong list, and a token whose query fingerprint does not match.
pub fn decode_page_token(raw: &str, canonical_query: &str) -> ApiResult<String> {
    let refuse = || ApiError::bad_request("That page marker is not valid. Search again.");

    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw.trim())
        .map_err(|_| refuse())?;
    // EDGE (schema drift): a token minted before `qf` existed has no such
    // field, so it fails to deserialise and is refused - which is the right
    // answer, because nothing can prove which query it belonged to.
    let payload: TokenPayload = serde_json::from_slice(&bytes).map_err(|_| refuse())?;
    if payload.pt.is_empty() {
        return Err(refuse());
    }
    if payload.qf != query_fingerprint(canonical_query) {
        return Err(ApiError::bad_request(
            "That page marker belongs to a different search. Search again.",
        ));
    }
    Ok(payload.pt)
}

/// A short, opaque digest of the canonical query.
///
/// Hashed rather than embedded: a cursor travels in URLs and logs, and the
/// query is the user's mail. 12 bytes is 96 bits - far past what a collision
/// would need, and a collision costs a confusing page, not a leak.
fn query_fingerprint(canonical_query: &str) -> String {
    use sha2::{Digest as _, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(canonical_query.as_bytes());
    let digest = hasher.finalize();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&digest[..12])
}

/// Turn a `limit + 1` fetch into one page and the cursor that follows it.
///
/// Every keyset list does the same three things — notice the extra row, drop
/// it, mint a cursor from the last row that survived — and each of them used to
/// spell it out again. `DECISIONS.md` D52 records a cursor-precision defect
/// that had already shipped in this epilogue; the next such fix should land in
/// one place, not four.
///
/// `key` names the ordering pair the query's `order by` uses. Getting it wrong
/// is the whole failure mode, so it is the argument the caller has to supply.
pub fn take_page<T>(
    rows: &mut Vec<T>,
    page: usize,
    key: impl Fn(&T) -> (DateTime<Utc>, String),
) -> Option<String> {
    // EDGE (pagination boundary): exactly `page` rows means the last page, and
    // `next_cursor` is null. `API.md` §0 - a cursor is minted only when a row
    // was actually left behind.
    if rows.len() <= page {
        return None;
    }
    rows.truncate(page);
    rows.last().map(|row| {
        let (ts, id) = key(row);
        encode(ts, &id)
    })
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
        let cursor = encode_page_token(token, "from:a@b.com");
        assert_eq!(decode_page_token(&cursor, "from:a@b.com").unwrap(), token);
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
            &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"pt":"","qf":"x"}"#),
            // EDGE (schema drift): the pre-fingerprint shape. Refused, because
            // nothing in it says which query it paged.
            &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"pt":"tok"}"#),
            // A keyset cursor, which is a position in a different kind of list.
            keyset.as_str(),
        ] {
            let error = decode_page_token(bad, "from:a@b.com").unwrap_err();
            assert_eq!(
                error.code,
                crate::error::ErrorCode::BadRequest,
                "{bad:?} should be a bad_request"
            );
        }

        // And the mirror: a page-token cursor is not a keyset cursor either.
        assert!(decode(&encode_page_token("tok", "from:a@b.com")).is_err());
    }

    /// Gmail's page token means nothing apart from the query that produced it.
    /// Pairing them wrongly is the interesting case: Gmail does not reliably
    /// reject it, so it can answer with a page of a *different* search - the
    /// user scrolls one query and is served another. `API.md` §0 requires 400.
    #[test]
    fn a_page_token_is_refused_with_a_different_query() {
        let cursor = encode_page_token("tok", "from:alice@example.com");

        let error = decode_page_token(&cursor, "from:bob@example.com").unwrap_err();
        assert_eq!(error.code, crate::error::ErrorCode::BadRequest);
        assert!(
            error.message.contains("different search"),
            "the message must say which of the two things went wrong: {:?}",
            error.message
        );

        // EDGE (empty input): the empty query is a query like any other, and
        // must not collide with a real one.
        assert!(decode_page_token(&encode_page_token("tok", ""), "in:inbox").is_err());
        assert!(decode_page_token(&encode_page_token("tok", "in:inbox"), "").is_err());

        // EDGE (unicode): a query is the user's own words, in their own script.
        let jp = encode_page_token("tok", "配送のお知らせ");
        assert_eq!(decode_page_token(&jp, "配送のお知らせ").unwrap(), "tok");
        assert!(decode_page_token(&jp, "配送のお知らせ ").is_err());
    }

    /// The fingerprint travels in URLs and server logs, so it must not carry
    /// the query back out again.
    #[test]
    fn the_fingerprint_does_not_leak_the_query() {
        let cursor = encode_page_token("tok", "from:oncologist@hospital.example");
        let decoded = String::from_utf8(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&cursor)
                .unwrap(),
        )
        .unwrap();
        assert!(
            !decoded.contains("oncologist") && !decoded.contains("hospital"),
            "the query must not be legible in the cursor: {decoded}"
        );
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

    /// Always `Z`, and **lossless**.
    ///
    /// This test used to assert the opposite - that a fractional second was
    /// truncated - on the grounds that "the iOS side decodes with a fixed
    /// formatter". That reasoning does not apply here: a cursor is opaque
    /// (`API.md` §0, "clients must not parse it"), the app stores it as a string
    /// and never base64-decodes one, and the `ts` inside it is not a wire
    /// timestamp. What it *is* is one half of a keyset comparison, so truncating
    /// it dropped every row inside the cursor's own second - `/runs` served an
    /// empty page two with 51 rows in the table.
    #[test]
    fn a_cursor_round_trips_its_timestamp_exactly() {
        let ts = at("2026-08-16T09:12:04.987654Z");
        assert_eq!(decode(&encode(ts, "x")).unwrap().ts, ts);
    }

    /// A whole second still encodes with no fractional part, so the contract
    /// fixtures - whose cursors are all whole seconds - are unaffected.
    #[test]
    fn a_whole_second_carries_no_fraction_and_is_z_suffixed() {
        let ts = at("2026-08-16T09:12:04Z");
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
