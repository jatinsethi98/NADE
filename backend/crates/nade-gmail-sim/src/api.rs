//! The request/response seam both transports share.
//!
//! The simulator's one implementation takes a [`SimRequest`] and returns a
//! [`SimResponse`]. The in-process transport calls it directly; the HTTP server
//! translates an `axum` request into one and its answer back out. Because the
//! seam is an *HTTP request* rather than a typed method, the two transports
//! cannot drift apart — there is nothing for them to disagree about.
//!
//! That choice also means the in-process path exercises URL building, query
//! encoding and header handling, which a typed façade would quietly skip.

use std::future::Future;

/// The HTTP methods the simulator answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    /// `GET`.
    Get,
    /// `POST`.
    Post,
    /// `DELETE`.
    Delete,
}

impl Method {
    /// The wire name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Delete => "DELETE",
        }
    }

    /// Parse a method name, case-insensitively.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "GET" => Some(Self::Get),
            "POST" => Some(Self::Post),
            "DELETE" => Some(Self::Delete),
            _ => None,
        }
    }
}

/// One request into the simulator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimRequest {
    /// The method.
    pub method: Method,
    /// The percent-decoded path, with no query string.
    pub path: String,
    /// Percent-decoded query parameters, in the order they appeared. A repeated
    /// parameter — `historyTypes` is always repeated — keeps every occurrence.
    pub query: Vec<(String, String)>,
    /// Headers, names lowercased.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

impl SimRequest {
    /// A `GET` for a path that may carry a query string.
    #[must_use]
    pub fn get(path_and_query: &str) -> Self {
        Self::new(Method::Get, path_and_query, Vec::new())
    }

    /// A `POST` with a body.
    #[must_use]
    pub fn post(path_and_query: &str, body: impl Into<Vec<u8>>) -> Self {
        Self::new(Method::Post, path_and_query, body.into())
    }

    /// Any method.
    #[must_use]
    pub fn new(method: Method, path_and_query: &str, body: Vec<u8>) -> Self {
        let (path, query) = split_target(path_and_query);
        Self {
            method,
            path,
            query,
            headers: Vec::new(),
            body,
        }
    }

    /// Attach an `Authorization: Bearer …` header.
    #[must_use]
    pub fn bearer(mut self, token: &str) -> Self {
        self.headers
            .push(("authorization".to_owned(), format!("Bearer {token}")));
        self
    }

    /// Attach any header. The name is lowercased.
    #[must_use]
    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_ascii_lowercase(), value.into()));
        self
    }

    /// The first value of a query parameter.
    #[must_use]
    pub fn query_one(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// Every value of a repeated query parameter, in order.
    #[must_use]
    pub fn query_all(&self, name: &str) -> Vec<&str> {
        self.query
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .collect()
    }

    /// A header value, matched case-insensitively.
    #[must_use]
    pub fn header_value(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The bearer token, if the request carries one.
    #[must_use]
    pub fn bearer_token(&self) -> Option<&str> {
        let raw = self.header_value("authorization")?;
        let (scheme, token) = raw.split_once(' ')?;
        scheme
            .eq_ignore_ascii_case("bearer")
            .then_some(token.trim())
    }

    /// The path and query as one string, re-encoded — used to build the request
    /// line of a batch sub-request.
    #[must_use]
    pub fn target(&self) -> String {
        if self.query.is_empty() {
            return self.path.clone();
        }
        let query = self
            .query
            .iter()
            .map(|(key, value)| format!("{}={}", encode_component(key), encode_component(value)))
            .collect::<Vec<_>>()
            .join("&");
        format!("{}?{query}", self.path)
    }
}

/// One response out of the simulator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimResponse {
    /// HTTP status.
    pub status: u16,
    /// Headers, names in the case Gmail uses.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
    /// How long a real Gmail would have taken, per any [`Delay`] fault.
    ///
    /// The in-process transport does **not** sleep — it reports this and returns
    /// at once, which keeps tests instant and deterministic. The HTTP transport
    /// really sleeps, because that is the only way a client's own timeout can
    /// fire.
    ///
    /// [`Delay`]: crate::fault::Fault::Delay
    pub latency: std::time::Duration,
}

impl SimResponse {
    /// A JSON response.
    #[must_use]
    pub fn json(status: u16, body: &serde_json::Value) -> Self {
        Self {
            status,
            headers: vec![(
                "Content-Type".to_owned(),
                "application/json; charset=UTF-8".to_owned(),
            )],
            body: serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec()),
            latency: std::time::Duration::ZERO,
        }
    }

    /// An empty `204`, which is what `users.stop` returns.
    #[must_use]
    pub fn no_content() -> Self {
        Self {
            status: 204,
            headers: Vec::new(),
            body: Vec::new(),
            latency: std::time::Duration::ZERO,
        }
    }

    /// A raw response with an explicit content type.
    #[must_use]
    pub fn raw(status: u16, content_type: &str, body: Vec<u8>) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".to_owned(), content_type.to_owned())],
            body,
            latency: std::time::Duration::ZERO,
        }
    }

    /// Add a header.
    #[must_use]
    pub fn with_header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_owned(), value.into()));
        self
    }

    /// Was it a 2xx?
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The body parsed as JSON, for tests.
    ///
    /// # Panics
    /// When the body is not JSON. A test that gets HTML back from what should be
    /// a JSON endpoint wants to know loudly.
    #[must_use]
    pub fn json_body(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|error| {
            panic!(
                "response body is not JSON ({error}): {}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    /// A header value, matched case-insensitively.
    #[must_use]
    pub fn header_value(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The `Content-Type`.
    #[must_use]
    pub fn content_type(&self) -> &str {
        self.header_value("content-type").unwrap_or_default()
    }
}

/// The seam a client can be generic over.
///
/// Implemented by the simulator, and implementable by a real HTTP client, so the
/// same sync code can be pointed at either. It is deliberately *not*
/// dyn-compatible: a generic bound costs nothing at runtime, and the in-process
/// simulator answers without ever yielding.
pub trait GmailApi: Send + Sync {
    /// Perform one call.
    fn call(&self, request: SimRequest) -> impl Future<Output = SimResponse> + Send;
}

// ---------------------------------------------------------------------------
// URL bits
// ---------------------------------------------------------------------------

/// Split `"/a/b?x=1&y=2"` into its decoded path and decoded parameters.
#[must_use]
pub fn split_target(target: &str) -> (String, Vec<(String, String)>) {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    (decode_component(path, false), parse_query(query))
}

/// Parse a query string into decoded pairs, keeping repeats and order.
#[must_use]
pub fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode_component(key, true), decode_component(value, true))
        })
        .collect()
}

/// Percent-decode. In a query string `+` also means a space; in a path it does
/// not, which is why the flag exists.
#[must_use]
pub fn decode_component(input: &str, plus_is_space: bool) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                // EDGE: a stray `%` that is not an escape. Pass it through
                // rather than dropping the rest of the value.
                if let Ok(byte) = u8::from_str_radix(&input[index + 1..index + 3], 16) {
                    out.push(byte);
                    index += 3;
                } else {
                    out.push(b'%');
                    index += 1;
                }
            }
            b'+' if plus_is_space => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encode everything outside the unreserved set.
#[must_use]
pub fn encode_component(input: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(*byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_splits_into_path_and_ordered_repeated_parameters() {
        let request = SimRequest::get(
            "/gmail/v1/users/me/history?startHistoryId=7&historyTypes=messageAdded&historyTypes=labelAdded",
        );
        assert_eq!(request.path, "/gmail/v1/users/me/history");
        assert_eq!(request.query_one("startHistoryId"), Some("7"));
        assert_eq!(
            request.query_all("historyTypes"),
            ["messageAdded", "labelAdded"],
            "a repeated parameter must keep every value, in order"
        );
    }

    #[test]
    fn percent_and_plus_decode_the_way_each_position_requires() {
        let request =
            SimRequest::get("/gmail/v1/users/me%40x.com/messages?q=newer_than%3A30d+is%3Aunread");
        assert_eq!(request.path, "/gmail/v1/users/me@x.com/messages");
        assert_eq!(request.query_one("q"), Some("newer_than:30d is:unread"));
        // `+` in a path is a literal plus.
        assert_eq!(decode_component("a+b", false), "a+b");
        assert_eq!(decode_component("a+b", true), "a b");
    }

    #[test]
    fn utf8_survives_percent_decoding() {
        let request = SimRequest::get("/x?q=subject%3ACaf%C3%A9");
        assert_eq!(request.query_one("q"), Some("subject:Café"));
    }

    #[test]
    fn malformed_escapes_do_not_eat_the_value() {
        assert_eq!(decode_component("100%", true), "100%");
        assert_eq!(decode_component("%zz", true), "%zz");
        assert_eq!(decode_component("a%2", true), "a%2");
    }

    #[test]
    fn a_target_round_trips_through_encoding() {
        let request =
            SimRequest::get("/gmail/v1/users/me/messages?q=newer_than%3A30d&maxResults=5");
        let rebuilt = SimRequest::get(&request.target());
        assert_eq!(rebuilt.query_one("q"), Some("newer_than:30d"));
        assert_eq!(rebuilt.query_one("maxResults"), Some("5"));
        assert_eq!(rebuilt.path, request.path);
    }

    #[test]
    fn bearer_tokens_are_read_case_insensitively_and_rejected_when_absent() {
        let request = SimRequest::get("/x").bearer("ya29.abc");
        assert_eq!(request.bearer_token(), Some("ya29.abc"));
        assert_eq!(SimRequest::get("/x").bearer_token(), None);
        assert_eq!(
            SimRequest::get("/x")
                .header("Authorization", "bearer lower")
                .bearer_token(),
            Some("lower")
        );
        assert_eq!(
            SimRequest::get("/x")
                .header("Authorization", "Basic abc")
                .bearer_token(),
            None
        );
    }

    #[test]
    fn an_empty_query_string_yields_no_parameters() {
        assert!(parse_query("").is_empty());
        assert!(parse_query("&&").is_empty());
        assert_eq!(parse_query("a"), [("a".to_owned(), String::new())]);
        assert_eq!(parse_query("a="), [("a".to_owned(), String::new())]);
    }

    #[test]
    fn methods_parse_and_render() {
        assert_eq!(Method::parse("get"), Some(Method::Get));
        assert_eq!(Method::parse(" POST "), Some(Method::Post));
        assert_eq!(Method::parse("PATCH"), None);
        assert_eq!(Method::Delete.as_str(), "DELETE");
    }

    #[test]
    fn responses_expose_their_headers_and_body() {
        let response =
            SimResponse::json(200, &serde_json::json!({ "a": 1 })).with_header("Retry-After", "5");
        assert!(response.is_success());
        assert_eq!(response.header_value("retry-after"), Some("5"));
        assert!(response.content_type().starts_with("application/json"));
        assert_eq!(response.json_body()["a"], 1);
        assert_eq!(SimResponse::no_content().status, 204);
    }
}
