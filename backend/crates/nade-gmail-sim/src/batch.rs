//! The `multipart/mixed` batch endpoint, parsed and generated for real.
//!
//! This is the single easiest place for a client to have a bug a naive
//! simulator would never reveal, so the simulator is deliberately unhelpful in
//! exactly the ways Google is:
//!
//! * **Sub-responses come back out of order by default.** Google states the
//!   order of the parts in the response is not guaranteed to match the requests,
//!   so a client must correlate by `Content-ID`. The simulator reverses them —
//!   deterministically, so nothing is flaky — and a client that zips the two
//!   lists fails on its first batch of two rather than in production under load.
//! * **A failing sub-request does not fail the batch.** The outer status is
//!   `200` and the dead sub-request is a `404` *part*. A message deleted between
//!   `messages.list` and `messages.get` is normal traffic, not an outage, and a
//!   client that treats the batch as all-or-nothing throws away every good
//!   message beside it.
//! * **`Content-ID` comes back with a `response-` prefix**, which is what Google
//!   does and what a client must therefore strip.

use crate::{
    api::{Method, SimRequest, SimResponse},
    ids::batch_boundary,
    mime::{parse_headers, split_header_body},
};

/// Google's documented ceiling on calls in one batch.
pub const MAX_BATCH_CALLS: usize = 100;

/// In what order sub-responses are returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BatchOrder {
    /// Reversed — the default, because "not guaranteed" must not read as
    /// "usually fine".
    #[default]
    Reversed,
    /// The order the sub-requests arrived in.
    AsRequested,
    /// Rotated left by one, so even a batch of two moves but neither end is
    /// where a naive client expects.
    Rotated,
}

impl BatchOrder {
    fn apply<T>(self, mut items: Vec<T>) -> Vec<T> {
        match self {
            Self::AsRequested => items,
            Self::Reversed => {
                items.reverse();
                items
            }
            Self::Rotated => {
                if !items.is_empty() {
                    items.rotate_left(1);
                }
                items
            }
        }
    }
}

/// One parsed sub-request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubRequest {
    /// The `Content-ID` as sent, angle brackets stripped.
    pub content_id: String,
    /// The request itself.
    pub request: SimRequest,
}

/// Why a batch body could not be read at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchParseError {
    /// The `Content-Type` was not `multipart/mixed`, or had no boundary.
    NotMultipart,
    /// The body contained no parts.
    Empty,
    /// More parts than Google accepts.
    TooManyCalls(usize),
}

impl BatchParseError {
    /// The message Gmail puts in the `400`.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::NotMultipart => {
                "Invalid multipart request. Content-Type must be multipart/mixed with a boundary."
                    .to_owned()
            }
            Self::Empty => "Batch request contained no parts.".to_owned(),
            Self::TooManyCalls(count) => format!(
                "Batch request contains {count} calls, which exceeds the limit of {MAX_BATCH_CALLS}."
            ),
        }
    }
}

/// Pull the boundary out of a `multipart/mixed; boundary=…` content type.
#[must_use]
pub fn boundary_of(content_type: &str) -> Option<String> {
    let (kind, params) = crate::mime::parse_parameterised(content_type);
    if !kind.eq_ignore_ascii_case("multipart/mixed") {
        return None;
    }
    params
        .into_iter()
        .find(|(name, _)| name == "boundary")
        .map(|(_, value)| value)
        .filter(|value| !value.is_empty())
}

/// Parse a batch request body into its sub-requests.
///
/// # Errors
/// [`BatchParseError`] when the content type is wrong, the body holds no parts,
/// or there are more than [`MAX_BATCH_CALLS`] of them.
pub fn parse_request(content_type: &str, body: &[u8]) -> Result<Vec<SubRequest>, BatchParseError> {
    let boundary = boundary_of(content_type).ok_or(BatchParseError::NotMultipart)?;
    let chunks = split_on_boundary(body, &boundary);
    if chunks.is_empty() {
        return Err(BatchParseError::Empty);
    }
    if chunks.len() > MAX_BATCH_CALLS {
        return Err(BatchParseError::TooManyCalls(chunks.len()));
    }

    let mut out = Vec::with_capacity(chunks.len());
    for (index, chunk) in chunks.into_iter().enumerate() {
        // A part the simulator cannot read at all still gets a slot, so the
        // response has as many parts as the request did. Dropping it would let a
        // client's "I got fewer answers than questions" bug go unseen.
        let parsed = parse_sub_request(chunk, index);
        out.push(parsed);
    }
    Ok(out)
}

fn parse_sub_request(chunk: &[u8], index: usize) -> SubRequest {
    let (header_block, rest) = split_header_body(chunk);
    let part_headers = parse_headers(header_block);
    let content_id = part_headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-id"))
        .map(|(_, value)| value.trim().trim_start_matches('<').trim_end_matches('>'))
        .filter(|value| !value.is_empty())
        // EDGE: a sub-request with no `Content-ID`. Google assigns one by
        // position; so does this, and the response carries it, which is the only
        // way the client can tell which answer it is.
        .map_or_else(|| format!("item-{index}"), ToOwned::to_owned);

    let rest = trim_leading_newlines(rest);
    let (request_line, after) = split_first_line(rest);
    let line = String::from_utf8_lossy(request_line);
    let mut fields = line.split_whitespace();
    let method = fields.next().and_then(Method::parse).unwrap_or(Method::Get);
    let target = fields.next().unwrap_or("/");

    let (inner_header_block, inner_body) = split_header_body(after);
    let mut request = SimRequest::new(method, target, inner_body.to_vec());
    for (name, value) in parse_headers(inner_header_block) {
        request = request.header(&name, value);
    }
    SubRequest {
        content_id,
        request,
    }
}

/// Build the batch response body. Returns `(content_type, body)`.
///
/// `seq` seeds the boundary, so the same script produces the same bytes.
#[must_use]
pub fn build_response(
    seq: u64,
    order: BatchOrder,
    parts: Vec<(String, SimResponse)>,
) -> (String, Vec<u8>) {
    let boundary = batch_boundary(seq);
    let mut out = Vec::new();
    for (content_id, response) in order.apply(parts) {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        out.extend_from_slice(b"Content-Type: application/http\r\n");
        // Google echoes the id with a `response-` prefix. A client that compares
        // it raw against what it sent will never match anything.
        out.extend_from_slice(format!("Content-ID: <response-{content_id}>\r\n\r\n").as_bytes());
        out.extend_from_slice(
            format!(
                "HTTP/1.1 {} {}\r\n",
                response.status,
                reason_phrase(response.status)
            )
            .as_bytes(),
        );
        for (name, value) in &response.headers {
            out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        out.extend_from_slice(
            format!("Content-Length: {}\r\n\r\n", response.body.len()).as_bytes(),
        );
        out.extend_from_slice(&response.body);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/mixed; boundary={boundary}"), out)
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

/// Split a multipart body into its parts, discarding preamble, epilogue and the
/// closing delimiter.
fn split_on_boundary<'a>(body: &'a [u8], boundary: &str) -> Vec<&'a [u8]> {
    let delimiter = format!("--{boundary}");
    let mut out: Vec<&[u8]> = Vec::new();
    let mut rest = body;
    let mut open = false;

    while let Some(at) = crate::mime::find(rest, delimiter.as_bytes()) {
        if open {
            out.push(trim_trailing_newlines(&rest[..at]));
        }
        let after = &rest[at + delimiter.len()..];
        if after.starts_with(b"--") {
            return out;
        }
        open = true;
        rest = trim_leading_newlines(after);
    }
    if open {
        out.push(trim_trailing_newlines(rest));
    }
    out
}

fn trim_leading_newlines(mut input: &[u8]) -> &[u8] {
    while let Some((first, rest)) = input.split_first() {
        if *first == b'\r' || *first == b'\n' {
            input = rest;
        } else {
            break;
        }
    }
    input
}

fn trim_trailing_newlines(mut input: &[u8]) -> &[u8] {
    while let Some((last, rest)) = input.split_last() {
        if *last == b'\r' || *last == b'\n' {
            input = rest;
        } else {
            break;
        }
    }
    input
}

fn split_first_line(input: &[u8]) -> (&[u8], &[u8]) {
    match crate::mime::find(input, b"\n") {
        Some(at) => {
            let end = if at > 0 && input[at - 1] == b'\r' {
                at - 1
            } else {
                at
            };
            (&input[..end], &input[at + 1..])
        }
        None => (input, &[]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Exactly the bytes `nade-server`'s client emits.
    fn client_body(boundary: &str, count: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for index in 0..count {
            out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            out.extend_from_slice(b"Content-Type: application/http\r\n");
            out.extend_from_slice(b"Content-Transfer-Encoding: binary\r\n");
            out.extend_from_slice(format!("Content-ID: <item-{index}>\r\n\r\n").as_bytes());
            out.extend_from_slice(
                format!("GET /gmail/v1/users/me/messages/msg{index}?format=raw HTTP/1.1\r\n\r\n")
                    .as_bytes(),
            );
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        out
    }

    #[test]
    fn a_real_clients_body_parses_into_its_sub_requests() {
        let body = client_body("nade_batch_test", 3);
        let parsed = parse_request("multipart/mixed; boundary=nade_batch_test", &body).unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].content_id, "item-0");
        assert_eq!(parsed[0].request.method, Method::Get);
        assert_eq!(parsed[0].request.path, "/gmail/v1/users/me/messages/msg0");
        assert_eq!(parsed[0].request.query_one("format"), Some("raw"));
        assert_eq!(parsed[2].content_id, "item-2");
    }

    #[test]
    fn a_quoted_boundary_and_lf_line_endings_both_work() {
        let body = b"--b\nContent-Type: application/http\nContent-ID: <a>\n\n\
                     GET /x?y=1 HTTP/1.1\n\n\
                     --b--\n";
        let parsed = parse_request("multipart/mixed; boundary=\"b\"", body).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].content_id, "a");
        assert_eq!(parsed[0].request.query_one("y"), Some("1"));
    }

    #[test]
    fn a_batch_of_one_is_still_a_batch() {
        let body = client_body("b", 1);
        let parsed = parse_request("multipart/mixed; boundary=b", &body).unwrap();
        assert_eq!(parsed.len(), 1);
        let (content_type, out) = build_response(
            1,
            BatchOrder::Reversed,
            vec![(
                "item-0".to_owned(),
                SimResponse::json(200, &json!({"id": "m"})),
            )],
        );
        assert!(content_type.starts_with("multipart/mixed; boundary=batch_nadesim_"));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("Content-ID: <response-item-0>"));
        assert!(text.contains("HTTP/1.1 200 OK"));
        assert!(text.ends_with("--\r\n"));
    }

    #[test]
    fn a_batch_of_one_hundred_parses_and_one_of_one_hundred_and_one_does_not() {
        let body = client_body("b", 100);
        assert_eq!(
            parse_request("multipart/mixed; boundary=b", &body)
                .unwrap()
                .len(),
            100
        );
        let body = client_body("b", 101);
        assert_eq!(
            parse_request("multipart/mixed; boundary=b", &body),
            Err(BatchParseError::TooManyCalls(101))
        );
    }

    #[test]
    fn an_empty_or_non_multipart_batch_is_rejected() {
        assert_eq!(
            parse_request("application/json", b"{}"),
            Err(BatchParseError::NotMultipart)
        );
        assert_eq!(
            parse_request("multipart/mixed", b"x"),
            Err(BatchParseError::NotMultipart)
        );
        assert_eq!(
            parse_request("multipart/mixed; boundary=b", b"--b--\r\n"),
            Err(BatchParseError::Empty)
        );
        assert_eq!(
            parse_request("multipart/mixed; boundary=b", b""),
            Err(BatchParseError::Empty)
        );
    }

    #[test]
    fn responses_come_back_reversed_by_default() {
        let parts: Vec<(String, SimResponse)> = (0..4)
            .map(|n| {
                (
                    format!("item-{n}"),
                    SimResponse::json(200, &json!({ "id": n })),
                )
            })
            .collect();
        let (_, out) = build_response(1, BatchOrder::default(), parts);
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.matches("Content-ID: <response-item-").count(), 4);
        let positions: Vec<usize> = (0..4)
            .map(|n| text.find(&format!("response-item-{n}")).unwrap())
            .collect();
        assert!(
            positions.windows(2).all(|pair| pair[0] > pair[1]),
            "the last request must answer first: {positions:?}"
        );
    }

    #[test]
    fn order_can_be_forced_when_a_test_needs_readable_output() {
        let parts: Vec<(String, SimResponse)> = (0..3)
            .map(|n| (format!("i{n}"), SimResponse::json(200, &json!({}))))
            .collect();
        let (_, out) = build_response(1, BatchOrder::AsRequested, parts.clone());
        let text = String::from_utf8(out).unwrap();
        assert!(text.find("response-i0").unwrap() < text.find("response-i2").unwrap());

        let (_, out) = build_response(1, BatchOrder::Rotated, parts);
        let text = String::from_utf8(out).unwrap();
        assert!(text.find("response-i1").unwrap() < text.find("response-i0").unwrap());
    }

    #[test]
    fn a_failing_part_carries_its_own_status_and_the_others_survive() {
        let mut parts: Vec<(String, SimResponse)> = (0..3)
            .map(|n| {
                (
                    format!("item-{n}"),
                    SimResponse::json(200, &json!({ "id": n })),
                )
            })
            .collect();
        parts[1].1 = SimResponse::json(404, &crate::ApiError::NotFound.body());
        let (_, out) = build_response(1, BatchOrder::AsRequested, parts);
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.matches("HTTP/1.1 200 OK").count(), 2);
        assert_eq!(text.matches("HTTP/1.1 404 Not Found").count(), 1);
        assert!(text.contains("\"reason\":\"notFound\""));
    }

    #[test]
    fn a_sub_request_with_no_content_id_gets_one_by_position() {
        let body = b"--b\r\nContent-Type: application/http\r\n\r\nGET /x HTTP/1.1\r\n\r\n--b--\r\n";
        let parsed = parse_request("multipart/mixed; boundary=b", body).unwrap();
        assert_eq!(parsed[0].content_id, "item-0");
    }

    #[test]
    fn a_garbled_part_still_gets_a_slot() {
        let body = b"--b\r\nContent-ID: <a>\r\n\r\nnot a request line\r\n\
                     --b\r\nContent-Type: application/http\r\nContent-ID: <c>\r\n\r\n\
                     GET /ok HTTP/1.1\r\n\r\n--b--\r\n";
        let parsed = parse_request("multipart/mixed; boundary=b", body).unwrap();
        assert_eq!(
            parsed.len(),
            2,
            "the client asked twice; it gets two answers"
        );
        assert_eq!(parsed[0].content_id, "a");
        assert_eq!(parsed[1].request.path, "/ok");
    }

    #[test]
    fn duplicate_content_ids_are_passed_through_not_deduplicated() {
        let body = b"--b\r\nContent-ID: <dup>\r\n\r\nGET /a HTTP/1.1\r\n\r\n\
                     --b\r\nContent-ID: <dup>\r\n\r\nGET /b HTTP/1.1\r\n\r\n--b--\r\n";
        let parsed = parse_request("multipart/mixed; boundary=b", body).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].content_id, parsed[1].content_id);
        // The simulator does not fix this. A client that indexes by Content-ID
        // will silently lose one answer, which is exactly the bug worth finding.
    }

    #[test]
    fn a_sub_request_can_carry_headers_and_a_body() {
        let body = b"--b\r\nContent-ID: <p>\r\n\r\n\
                     POST /gmail/v1/users/me/watch HTTP/1.1\r\n\
                     Content-Type: application/json\r\n\r\n\
                     {\"topicName\":\"t\"}\r\n--b--\r\n";
        let parsed = parse_request("multipart/mixed; boundary=b", body).unwrap();
        assert_eq!(parsed[0].request.method, Method::Post);
        assert_eq!(
            parsed[0].request.header_value("content-type"),
            Some("application/json")
        );
        assert_eq!(parsed[0].request.body, b"{\"topicName\":\"t\"}");
    }

    #[test]
    fn boundaries_are_read_bare_or_quoted_and_only_for_multipart_mixed() {
        assert_eq!(
            boundary_of("multipart/mixed; boundary=abc").as_deref(),
            Some("abc")
        );
        assert_eq!(
            boundary_of("multipart/mixed; boundary=\"a b\"").as_deref(),
            Some("a b")
        );
        assert_eq!(boundary_of("multipart/related; boundary=abc"), None);
        assert_eq!(boundary_of("multipart/mixed"), None);
        assert_eq!(boundary_of(""), None);
    }

    #[test]
    fn generated_bodies_are_byte_identical_across_calls() {
        let parts = vec![("a".to_owned(), SimResponse::json(200, &json!({"x": 1})))];
        assert_eq!(
            build_response(9, BatchOrder::Reversed, parts.clone()),
            build_response(9, BatchOrder::Reversed, parts)
        );
    }

    #[test]
    fn unicode_bodies_survive_the_multipart_round_trip() {
        let body = json!({ "snippet": "Föhn — 配送 📦" });
        let (content_type, out) = build_response(
            1,
            BatchOrder::AsRequested,
            vec![("a".to_owned(), SimResponse::json(200, &body))],
        );
        assert!(content_type.contains("boundary="));
        let text = String::from_utf8(out).expect("valid UTF-8");
        assert!(text.contains("Föhn"), "{text}");
    }
}
