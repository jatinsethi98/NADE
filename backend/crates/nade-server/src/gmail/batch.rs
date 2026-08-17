//! Gmail's `multipart/mixed` batch endpoint.
//!
//! Two things everybody gets wrong, and both are load-bearing here:
//!
//! 1. **Responses are correlated by `Content-ID`, not by order.** Google is
//!    explicit that the order of the parts in the response is not guaranteed to
//!    match the order of the requests. Zipping the two lists is a bug that only
//!    shows up under load, as messages ingested against the wrong id.
//! 2. **One failing sub-request must not fail the batch.** A message deleted
//!    between `messages.list` and `messages.get` comes back as a `404` *part*
//!    inside a `200` batch. Treating the batch as all-or-nothing throws away 44
//!    good messages because one was deleted.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

/// One sub-request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchRequest {
    /// Our correlation key. Echoed by Google, usually with a `response-` prefix.
    pub content_id: String,
    pub method: &'static str,
    /// Path **and** query, relative to the API root: `/gmail/v1/users/me/...`.
    pub path: String,
}

/// One sub-response, already matched back to its request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchResponse {
    pub content_id: String,
    pub status: u16,
    pub body: Vec<u8>,
}

impl BatchResponse {
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// A message the user deleted between the list and the get. Normal, not an
    /// error: skip it, do not retry it, do not fail the sync.
    #[must_use]
    pub fn is_gone(&self) -> bool {
        self.status == 404 || self.status == 410
    }
}

/// A boundary no message body can collide with.
#[must_use]
pub fn fresh_boundary() -> String {
    format!("nade_batch_{}", uuid::Uuid::new_v4().simple())
}

/// Build the `multipart/mixed` request body.
#[must_use]
pub fn build_body(boundary: &str, requests: &[BatchRequest]) -> Vec<u8> {
    let mut out = Vec::with_capacity(requests.len() * 160);
    for request in requests {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        out.extend_from_slice(b"Content-Type: application/http\r\n");
        out.extend_from_slice(b"Content-Transfer-Encoding: binary\r\n");
        out.extend_from_slice(format!("Content-ID: <{}>\r\n\r\n", request.content_id).as_bytes());
        out.extend_from_slice(
            format!("{} {} HTTP/1.1\r\n\r\n", request.method, request.path).as_bytes(),
        );
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    out
}

/// Pull the boundary out of a `multipart/mixed; boundary=…` content type.
#[must_use]
pub fn boundary_from_content_type(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.split_once('=')?;
        if !name.trim().eq_ignore_ascii_case("boundary") {
            return None;
        }
        let value = value.trim().trim_matches('"').trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

/// Parse a batch response into sub-responses, keyed by our `Content-ID`.
///
/// # Errors
/// Returns an error when the content type is not multipart, the boundary is
/// missing, or no parts can be recovered at all - a genuinely malformed
/// response, which is a retryable failure rather than a silent empty result.
pub fn parse_response(content_type: &str, body: &[u8]) -> Result<Vec<BatchResponse>> {
    if !content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .eq_ignore_ascii_case("multipart/mixed")
    {
        bail!("batch response is {content_type:?}, not multipart/mixed");
    }
    let boundary = boundary_from_content_type(content_type)
        .context("batch response has no multipart boundary")?;

    let delimiter = format!("--{boundary}");
    let mut responses = Vec::new();
    let mut saw_a_part = false;

    for chunk in split_on(body, delimiter.as_bytes()).into_iter().skip(1) {
        // The terminating `--` closes the multipart; anything after it is
        // epilogue.
        if chunk.starts_with(b"--") {
            break;
        }
        saw_a_part = true;
        if let Some(parsed) = parse_part(chunk) {
            responses.push(parsed);
        }
    }

    if !saw_a_part {
        bail!("batch response contained no parts");
    }
    Ok(responses)
}

/// One `application/http` part: our headers, a blank line, then a whole HTTP
/// response.
fn parse_part(chunk: &[u8]) -> Option<BatchResponse> {
    let chunk = trim_leading_crlf(chunk);
    let (part_headers, inner) = split_headers(chunk)?;
    let content_id = header_value(&part_headers, "content-id")?;

    let inner = trim_leading_crlf(inner);
    let (status_line, rest) = split_line(inner)?;
    let status = parse_status(&String::from_utf8_lossy(status_line))?;

    // The inner response's own headers, then its body.
    let body = split_headers(rest).map_or_else(Vec::new, |(_, body)| body.to_vec());

    Some(BatchResponse {
        content_id: normalise_content_id(&content_id),
        status,
        body: trim_trailing_crlf(&body).to_vec(),
    })
}

/// Google echoes our `Content-ID` as `<response-item-3>`; strip the brackets and
/// the prefix so it matches what we sent.
fn normalise_content_id(raw: &str) -> String {
    let trimmed = raw.trim().trim_start_matches('<').trim_end_matches('>');
    trimmed
        .strip_prefix("response-")
        .unwrap_or(trimmed)
        .to_owned()
}

fn parse_status(line: &str) -> Option<u16> {
    let mut fields = line.split_whitespace();
    let version = fields.next()?;
    if !version.to_ascii_uppercase().starts_with("HTTP/") {
        return None;
    }
    fields.next()?.parse().ok()
}

fn split_headers(input: &[u8]) -> Option<(Vec<(String, String)>, &[u8])> {
    let end = find(input, b"\r\n\r\n")
        .map(|at| (at, at + 4))
        .or_else(|| find(input, b"\n\n").map(|at| (at, at + 2)))?;
    let raw = String::from_utf8_lossy(&input[..end.0]);
    let headers = raw
        .lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Some((headers, &input[end.1..]))
}

fn header_value(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

fn split_line(input: &[u8]) -> Option<(&[u8], &[u8])> {
    let at = find(input, b"\n")?;
    let line_end = if at > 0 && input[at - 1] == b'\r' { at - 1 } else { at };
    Some((&input[..line_end], &input[at + 1..]))
}

fn trim_leading_crlf(mut input: &[u8]) -> &[u8] {
    while let Some((first, rest)) = input.split_first() {
        if *first == b'\r' || *first == b'\n' {
            input = rest;
        } else {
            break;
        }
    }
    input
}

fn trim_trailing_crlf(mut input: &[u8]) -> &[u8] {
    while let Some((last, rest)) = input.split_last() {
        if *last == b'\r' || *last == b'\n' {
            input = rest;
        } else {
            break;
        }
    }
    input
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn split_on<'a>(input: &'a [u8], separator: &[u8]) -> Vec<&'a [u8]> {
    let mut out = Vec::new();
    let mut rest = input;
    while let Some(at) = find(rest, separator) {
        out.push(&rest[..at]);
        rest = &rest[at + separator.len()..];
    }
    out.push(rest);
    out
}

/// Index the sub-responses by our `Content-ID`.
///
/// This is the whole point: the caller looks its request up rather than trusting
/// position.
#[must_use]
pub fn index_by_content_id(responses: Vec<BatchResponse>) -> HashMap<String, BatchResponse> {
    responses
        .into_iter()
        .map(|response| (response.content_id.clone(), response))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requests(count: usize) -> Vec<BatchRequest> {
        (0..count)
            .map(|index| BatchRequest {
                content_id: format!("item-{index}"),
                method: "GET",
                path: format!("/gmail/v1/users/me/messages/msg{index}?format=raw"),
            })
            .collect()
    }

    /// Builds a response whose parts are in `order`, so a test can shuffle them.
    fn response_body(boundary: &str, order: &[usize], statuses: &[(usize, u16)]) -> Vec<u8> {
        let mut out = String::new();
        for index in order {
            let status = statuses
                .iter()
                .find(|(id, _)| id == index)
                .map_or(200, |(_, status)| *status);
            let payload = if status == 200 {
                format!(r#"{{"id":"msg{index}","raw":"cGF5bG9hZC17aW5kZXh9"}}"#)
            } else {
                format!(r#"{{"error":{{"code":{status},"message":"gone"}}}}"#)
            };
            out.push_str(&format!(
                "--{boundary}\r\n\
                 Content-Type: application/http\r\n\
                 Content-ID: <response-item-{index}>\r\n\r\n\
                 HTTP/1.1 {status} OK\r\n\
                 Content-Type: application/json; charset=UTF-8\r\n\
                 Content-Length: {}\r\n\r\n\
                 {payload}\r\n",
                payload.len()
            ));
        }
        out.push_str(&format!("--{boundary}--\r\n"));
        out.into_bytes()
    }

    /// Criterion M3.
    #[test]
    fn request_body_is_multipart_mixed() {
        let boundary = "nade_batch_test";
        let body = String::from_utf8(build_body(boundary, &requests(2))).unwrap();

        assert!(body.starts_with("--nade_batch_test\r\n"), "{body}");
        assert!(body.ends_with("--nade_batch_test--\r\n"), "{body}");
        assert_eq!(body.matches("Content-Type: application/http").count(), 2);
        assert!(body.contains("Content-ID: <item-0>"));
        assert!(body.contains("Content-ID: <item-1>"));
        assert!(body.contains("GET /gmail/v1/users/me/messages/msg0?format=raw HTTP/1.1"));
        // Each sub-request's headers end with a blank line, then the request
        // line, then another blank line - Google rejects anything else.
        assert!(body.contains("Content-ID: <item-0>\r\n\r\nGET "));
    }

    /// Criterion M4 - the correlation trap. The response deliberately arrives
    /// backwards, with one hole, and every id must still land on its own data.
    #[test]
    fn out_of_order_responses_are_correlated_by_content_id() {
        let boundary = "b1";
        let shuffled = [3usize, 0, 4, 2, 1];
        let body = response_body(boundary, &shuffled, &[]);

        let parsed = parse_response(&format!("multipart/mixed; boundary={boundary}"), &body).unwrap();
        assert_eq!(parsed.len(), 5);

        let indexed = index_by_content_id(parsed);
        for index in 0..5 {
            let response = &indexed[&format!("item-{index}")];
            assert_eq!(response.status, 200);
            let value: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
            assert_eq!(
                value["id"], format!("msg{index}"),
                "item-{index} was matched to the wrong response"
            );
        }
    }

    /// Criterion M5 - the 45-message batch that contains one deleted message.
    #[test]
    fn a_single_404_does_not_fail_the_batch() {
        let boundary = "b2";
        let order: Vec<usize> = (0..45).collect();
        let body = response_body(boundary, &order, &[(17, 404)]);

        let parsed =
            parse_response(&format!("multipart/mixed; boundary=\"{boundary}\""), &body).unwrap();
        assert_eq!(parsed.len(), 45, "every sub-response must come back");

        let indexed = index_by_content_id(parsed);
        let good = indexed.values().filter(|r| r.is_success()).count();
        let gone = indexed.values().filter(|r| r.is_gone()).count();
        assert_eq!(good, 44, "the other 44 results must survive");
        assert_eq!(gone, 1);
        assert!(indexed["item-17"].is_gone());
        assert!(!indexed["item-17"].is_success());
    }

    /// Criterion M6.
    #[test]
    fn a_malformed_response_is_an_error_not_a_panic() {
        assert!(parse_response("application/json", b"{}").is_err());
        assert!(parse_response("multipart/mixed", b"nothing").is_err());
        assert!(parse_response("multipart/mixed; boundary=b", b"").is_err());
        assert!(parse_response("", b"").is_err());

        // A part with no HTTP status line is dropped rather than crashing, and
        // the rest of the batch survives.
        let body = b"--b\r\nContent-ID: <item-0>\r\n\r\ngarbage\r\n\
                     --b\r\nContent-Type: application/http\r\nContent-ID: <item-1>\r\n\r\n\
                     HTTP/1.1 200 OK\r\n\r\n{}\r\n--b--\r\n";
        let parsed = parse_response("multipart/mixed; boundary=b", body).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].content_id, "item-1");
    }

    /// Criterion M7 - zero requests must never become an HTTP call. The caller
    /// checks `is_empty()`; this pins the body it would otherwise send.
    #[test]
    fn an_empty_batch_is_just_the_terminator() {
        let body = String::from_utf8(build_body("b", &[])).unwrap();
        assert_eq!(body, "--b--\r\n");
    }

    /// Criterion M8.
    #[test]
    fn unicode_survives_the_round_trip() {
        let boundary = "b3";
        let payload = r#"{"id":"m","snippet":"Föhn — Ihre Bestellung ist unterwegs 📦 配送"}"#;
        let body = format!(
            "--{boundary}\r\nContent-Type: application/http\r\nContent-ID: <response-item-0>\r\n\r\n\
             HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{payload}\r\n--{boundary}--\r\n"
        );
        let parsed =
            parse_response(&format!("multipart/mixed; boundary={boundary}"), body.as_bytes())
                .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&parsed[0].body).unwrap();
        assert_eq!(
            value["snippet"],
            "Föhn — Ihre Bestellung ist unterwegs 📦 配送"
        );
    }

    #[test]
    fn boundaries_are_read_quoted_or_bare() {
        assert_eq!(
            boundary_from_content_type("multipart/mixed; boundary=batch_abc"),
            Some("batch_abc".to_owned())
        );
        assert_eq!(
            boundary_from_content_type(r#"multipart/mixed; boundary="batch_abc""#),
            Some("batch_abc".to_owned())
        );
        assert_eq!(
            boundary_from_content_type("multipart/mixed; charset=utf-8; BOUNDARY = x"),
            Some("x".to_owned())
        );
        assert_eq!(boundary_from_content_type("multipart/mixed"), None);
        assert_eq!(boundary_from_content_type(""), None);
    }

    /// LF-only line endings, which some proxies produce.
    #[test]
    fn lf_only_parts_still_parse() {
        let body = b"--b\nContent-Type: application/http\nContent-ID: <response-item-0>\n\n\
                     HTTP/1.1 200 OK\nContent-Type: application/json\n\n{\"id\":\"m\"}\n--b--\n";
        let parsed = parse_response("multipart/mixed; boundary=b", body).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].status, 200);
        assert_eq!(parsed[0].body, b"{\"id\":\"m\"}");
    }

    /// A duplicated `Content-ID` must not silently multiply into two ingests.
    #[test]
    fn duplicate_content_ids_collapse_to_one() {
        let boundary = "b4";
        let body = response_body(boundary, &[0, 0, 1], &[]);
        let parsed = parse_response(&format!("multipart/mixed; boundary={boundary}"), &body).unwrap();
        assert_eq!(parsed.len(), 3);
        let indexed = index_by_content_id(parsed);
        assert_eq!(indexed.len(), 2);
    }
}
