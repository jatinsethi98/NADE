//! The Gmail REST client: raw `reqwest`, no Google SDK.
//!
//! Every call goes through one place, [`GmailClient::send`], so the quota debit,
//! the bearer token, the 401-refresh and the 429 backoff cannot be forgotten on
//! a new endpoint.

use std::sync::Arc;

use base64::Engine as _;
use serde::de::DeserializeOwned;

use super::{
    batch::{self, BatchRequest, BatchResponse},
    oauth::{AccessTokens, TokenError},
    quota::{self, Bucket},
    types::{
        ApiErrorEnvelope, Attachment, GmailMessage, HistoryList, Label, LabelsList, MessageRef,
        MessagesList, Profile,
    },
};

/// Gmail caps a batch at 100 sub-requests; PLAN.md §Gmail sync pins ours at 45,
/// which is 225 quota units - comfortably inside the 250/s ceiling with room for
/// the odd retry.
pub const MAX_BATCH: usize = 45;

/// `messages.list` page size. Gmail's maximum is 500.
const LIST_PAGE: usize = 500;

#[derive(Debug, thiserror::Error)]
pub enum GmailError {
    /// The message is gone. Normal between `messages.list` and `messages.get`:
    /// the user deleted it. Skip it; never retry it.
    #[error("gmail no longer has that resource")]
    NotFound,
    #[error("the Gmail credential is dead; the user must re-consent")]
    NeedsReauth,
    #[error("gmail: {0}")]
    Upstream(String),
}

impl From<TokenError> for GmailError {
    fn from(error: TokenError) -> Self {
        match error {
            TokenError::NeedsReauth => Self::NeedsReauth,
            other => Self::Upstream(other.to_string()),
        }
    }
}

/// Where the API lives. Overridable so the wiremock suite can be the API.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub api_base: String,
    pub batch_url: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self::google()
    }
}

impl Endpoints {
    #[must_use]
    pub fn google() -> Self {
        Self {
            api_base: "https://gmail.googleapis.com".to_owned(),
            batch_url: "https://gmail.googleapis.com/batch/gmail/v1".to_owned(),
        }
    }

    /// Point everything at one host - what the tests do.
    #[must_use]
    pub fn at(base: &str) -> Self {
        let base = base.trim_end_matches('/').to_owned();
        Self {
            batch_url: format!("{base}/batch/gmail/v1"),
            api_base: base,
        }
    }
}

pub struct GmailClient {
    http: reqwest::Client,
    endpoints: Endpoints,
    quota: Arc<Bucket>,
    tokens: Arc<dyn AccessTokens>,
    max_attempts: u32,
    /// Multiplies every computed backoff. Production leaves it at 1.0; the
    /// wiremock suite sets it tiny so a 429 test asserts the *schedule* without
    /// spending a real minute asleep.
    backoff_scale: f64,
}

impl std::fmt::Debug for GmailClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GmailClient")
            .field("endpoints", &self.endpoints)
            .field("max_attempts", &self.max_attempts)
            .finish_non_exhaustive()
    }
}

impl GmailClient {
    #[must_use]
    pub fn new(
        http: reqwest::Client,
        endpoints: Endpoints,
        quota: Arc<Bucket>,
        tokens: Arc<dyn AccessTokens>,
    ) -> Self {
        Self {
            http,
            endpoints,
            quota,
            tokens,
            max_attempts: 6,
            backoff_scale: 1.0,
        }
    }

    #[must_use]
    pub fn with_retry_budget(mut self, max_attempts: u32, backoff_scale: f64) -> Self {
        self.max_attempts = max_attempts.max(1);
        self.backoff_scale = backoff_scale.max(0.0);
        self
    }

    // ------------------------------------------------------- endpoints --

    /// `users.getProfile`. Read **first**, so its `historyId` predates the list
    /// and a message arriving mid-sync causes an overlap rather than a gap.
    ///
    /// # Errors
    /// Returns [`GmailError`] on an upstream failure or a dead credential.
    pub async fn get_profile(&self) -> Result<Profile, GmailError> {
        self.get_json("/gmail/v1/users/me/profile", quota::cost::GET_PROFILE)
            .await
    }

    /// `users.labels.list`.
    ///
    /// # Errors
    /// Returns [`GmailError`] on an upstream failure.
    pub async fn list_labels(&self) -> Result<Vec<Label>, GmailError> {
        let list: LabelsList = self
            .get_json("/gmail/v1/users/me/labels", quota::cost::LABELS_LIST)
            .await?;
        Ok(list.labels)
    }

    /// `users.messages.list`, following `nextPageToken` until the cap.
    ///
    /// The cap is a dev law (`MAX_SYNC_MESSAGES`), so it is enforced here rather
    /// than trusted to the caller.
    ///
    /// # Errors
    /// Returns [`GmailError`] on an upstream failure.
    pub async fn list_message_ids(
        &self,
        query: &str,
        cap: usize,
    ) -> Result<Vec<MessageRef>, GmailError> {
        let mut out: Vec<MessageRef> = Vec::new();
        let mut page_token: Option<String> = None;

        while out.len() < cap {
            let want = (cap - out.len()).min(LIST_PAGE);
            let mut path = format!(
                "/gmail/v1/users/me/messages?maxResults={want}&q={}",
                encode(query)
            );
            if let Some(token) = &page_token {
                path.push_str(&format!("&pageToken={}", encode(token)));
            }

            let page: MessagesList = self.get_json(&path, quota::cost::MESSAGES_LIST).await?;
            let empty = page.messages.is_empty();
            out.extend(page.messages);

            page_token = page.next_page_token;
            // EDGE (pagination boundary): stop on no token *and* on a page that
            // returned nothing, so a server that always echoes a token cannot
            // spin us forever.
            if page_token.is_none() || empty {
                break;
            }
        }

        out.truncate(cap);
        Ok(out)
    }

    /// `users.history.list` from a stored cursor.
    ///
    /// Gmail keeps roughly a week of history. Past that it answers `404`, which
    /// is not an error but an instruction: **re-run the full 30-day sync**
    /// (PLAN.md §Gmail sync 4). Returning [`GmailError::NotFound`] here is what
    /// lets P3 tell "nothing changed" apart from "your cursor is too old".
    ///
    /// # Errors
    /// [`GmailError::NotFound`] when `start_history_id` is beyond Gmail's
    /// retention.
    pub async fn list_history(
        &self,
        start_history_id: i64,
        page_token: Option<&str>,
    ) -> Result<HistoryList, GmailError> {
        let mut path = format!(
            "/gmail/v1/users/me/history?startHistoryId={start_history_id}\
             &historyTypes=messageAdded&historyTypes=messageDeleted\
             &historyTypes=labelAdded&historyTypes=labelRemoved"
        );
        if let Some(token) = page_token {
            path.push_str(&format!("&pageToken={}", encode(token)));
        }
        self.get_json(&path, quota::cost::HISTORY_LIST).await
    }

    /// `users.messages.get?format=raw` - the whole RFC-822 message.
    ///
    /// # Errors
    /// [`GmailError::NotFound`] when the message vanished between list and get.
    pub async fn get_message_raw(&self, id: &str) -> Result<GmailMessage, GmailError> {
        self.get_json(
            &format!("/gmail/v1/users/me/messages/{}?format=raw", encode(id)),
            quota::cost::MESSAGES_GET,
        )
        .await
    }

    /// `users.messages.get?format=metadata` - headers only, no body.
    ///
    /// # Errors
    /// [`GmailError::NotFound`] when the message is gone.
    pub async fn get_message_metadata(&self, id: &str) -> Result<GmailMessage, GmailError> {
        self.get_json(
            &format!("/gmail/v1/users/me/messages/{}?format=metadata", encode(id)),
            quota::cost::MESSAGES_GET,
        )
        .await
    }

    /// `users.messages.get?format=full` - the part tree, which is the only place
    /// Gmail's `attachmentId` appears. `format=raw` does not carry it at all.
    ///
    /// # Errors
    /// [`GmailError::NotFound`] when the message is gone.
    pub async fn get_message_full(&self, id: &str) -> Result<GmailMessage, GmailError> {
        self.get_json(
            &format!("/gmail/v1/users/me/messages/{}?format=full", encode(id)),
            quota::cost::MESSAGES_GET,
        )
        .await
    }

    /// `users.messages.attachments.get`, decoded.
    ///
    /// # Errors
    /// [`GmailError::NotFound`] when Gmail no longer has the message.
    pub async fn get_attachment(
        &self,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<Vec<u8>, GmailError> {
        let attachment: Attachment = self
            .get_json(
                &format!(
                    "/gmail/v1/users/me/messages/{}/attachments/{}",
                    encode(message_id),
                    encode(attachment_id)
                ),
                quota::cost::ATTACHMENTS_GET,
            )
            .await?;
        decode_base64url(&attachment.data)
            .ok_or_else(|| GmailError::Upstream("attachment body was not base64url".to_owned()))
    }

    /// One real `multipart/mixed` batch of `messages.get?format=raw`.
    ///
    /// Returns one outcome **per requested id**, correlated by `Content-ID` and
    /// never by position. A sub-request that 404s yields
    /// [`BatchOutcome::Gone`]; the other 44 still come back.
    ///
    /// # Errors
    /// Returns [`GmailError`] only when the *whole* batch failed - a transport
    /// error, an auth failure, or a response that is not multipart at all.
    pub async fn batch_get_raw(&self, ids: &[String]) -> Result<Vec<BatchOutcome>, GmailError> {
        // EDGE (empty input): zero requests makes zero HTTP calls.
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        if ids.len() > MAX_BATCH {
            return Err(GmailError::Upstream(format!(
                "a batch may hold at most {MAX_BATCH} sub-requests, got {}",
                ids.len()
            )));
        }

        let requests: Vec<BatchRequest> = ids
            .iter()
            .enumerate()
            .map(|(index, id)| BatchRequest {
                content_id: format!("item-{index}"),
                method: "GET",
                path: format!("/gmail/v1/users/me/messages/{}?format=raw", encode(id)),
            })
            .collect();

        let boundary = batch::fresh_boundary();
        let body = batch::build_body(&boundary, &requests);
        let cost = quota::cost::MESSAGES_GET * u32::try_from(ids.len()).unwrap_or(u32::MAX);

        let (content_type, raw) = self
            .send(
                reqwest::Method::POST,
                &self.endpoints.batch_url.clone(),
                Some((format!("multipart/mixed; boundary={boundary}"), body)),
                cost,
            )
            .await?;

        let parsed = batch::parse_response(&content_type, &raw)
            .map_err(|error| GmailError::Upstream(format!("{error:#}")))?;
        let mut indexed = batch::index_by_content_id(parsed);

        Ok(ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let key = format!("item-{index}");
                match indexed.remove(&key) {
                    Some(response) => outcome(id, &response),
                    // A sub-response we never got back. Treat it as a soft
                    // failure so the caller retries the message alone rather
                    // than losing it.
                    None => BatchOutcome::Failed {
                        gmail_id: id.clone(),
                        status: 0,
                        detail: "no sub-response carried this Content-ID".to_owned(),
                    },
                }
            })
            .collect())
    }

    // --------------------------------------------------------- plumbing --

    async fn get_json<T: DeserializeOwned>(&self, path: &str, cost: u32) -> Result<T, GmailError> {
        let url = format!("{}{path}", self.endpoints.api_base);
        let (_, body) = self.send(reqwest::Method::GET, &url, None, cost).await?;
        serde_json::from_slice(&body).map_err(|error| {
            GmailError::Upstream(format!(
                "gmail returned unreadable JSON for {path}: {error}"
            ))
        })
    }

    /// One request, with the quota debit, the bearer token, and the retry
    /// schedule. Returns the response content type and body.
    async fn send(
        &self,
        method: reqwest::Method,
        url: &str,
        body: Option<(String, Vec<u8>)>,
        cost: u32,
    ) -> Result<(String, Vec<u8>), GmailError> {
        let mut attempt = 0u32;
        let mut auth_retries = 0u32;

        loop {
            self.quota.acquire(cost).await;
            let token = self.tokens.access_token().await?;

            let mut request = self
                .http
                .request(method.clone(), url)
                .bearer_auth(token)
                .header(reqwest::header::ACCEPT, "application/json");
            if let Some((content_type, payload)) = &body {
                request = request
                    .header(reqwest::header::CONTENT_TYPE, content_type)
                    .body(payload.clone());
            }

            let outcome = request.send().await;
            let retry_after;
            let status;
            let content_type;
            let payload;

            match outcome {
                Ok(response) => {
                    status = response.status();
                    retry_after = response
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|value| value.to_str().ok())
                        .and_then(quota::parse_retry_after);
                    content_type = response
                        .headers()
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_owned();
                    payload = response
                        .bytes()
                        .await
                        .map_err(|error| GmailError::Upstream(error.to_string()))?
                        .to_vec();

                    if status.is_success() {
                        return Ok((content_type, payload));
                    }
                    if status == reqwest::StatusCode::NOT_FOUND
                        || status == reqwest::StatusCode::GONE
                    {
                        return Err(GmailError::NotFound);
                    }
                    if status == reqwest::StatusCode::UNAUTHORIZED {
                        // The access token died early. Refresh once, then treat
                        // a second 401 as a dead credential rather than looping.
                        auth_retries += 1;
                        if auth_retries > 1 {
                            return Err(GmailError::NeedsReauth);
                        }
                        self.tokens.invalidate().await;
                        continue;
                    }
                    if !self.is_retryable(status, &payload) {
                        return Err(GmailError::Upstream(format!(
                            "{status}: {}",
                            String::from_utf8_lossy(&payload)
                                .chars()
                                .take(300)
                                .collect::<String>()
                        )));
                    }
                }
                Err(error) => {
                    // EDGE (timeout): a transport failure is retryable on the
                    // same schedule as a 429. This arm always diverges, so
                    // `status`/`content_type`/`payload` are never read unset.
                    if attempt + 1 >= self.max_attempts {
                        return Err(GmailError::Upstream(error.to_string()));
                    }
                    self.wait(attempt, None).await;
                    attempt += 1;
                    continue;
                }
            }

            attempt += 1;
            if attempt >= self.max_attempts {
                return Err(GmailError::Upstream(format!(
                    "{status} after {attempt} attempts"
                )));
            }
            self.wait(attempt - 1, retry_after).await;
        }
    }

    fn is_retryable(&self, status: reqwest::StatusCode, body: &[u8]) -> bool {
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            return true;
        }
        // 403 is the ambiguous one: `rateLimitExceeded` means slow down,
        // `insufficientPermissions` means never.
        status == reqwest::StatusCode::FORBIDDEN && ApiErrorEnvelope::parse(body).is_rate_limit()
    }

    async fn wait(&self, attempt: u32, retry_after: Option<std::time::Duration>) {
        let delay = quota::backoff(attempt, retry_after, quota::random_jitter());
        let scaled = delay.mul_f64(self.backoff_scale);
        tracing::warn!(
            attempt,
            wait_ms = scaled.as_millis(),
            "gmail throttled or failed; backing off"
        );
        if !scaled.is_zero() {
            tokio::time::sleep(scaled).await;
        }
    }
}

/// What happened to one message inside a batch.
#[derive(Debug, Clone)]
pub enum BatchOutcome {
    Fetched {
        gmail_id: String,
        message: Box<GmailMessage>,
        /// The decoded RFC-822 bytes, ready for the parser.
        raw: Vec<u8>,
    },
    /// The user deleted it between the list and the get. Not an error.
    Gone { gmail_id: String },
    /// Something else went wrong with this one sub-request only.
    Failed {
        gmail_id: String,
        status: u16,
        detail: String,
    },
}

impl BatchOutcome {
    #[must_use]
    pub fn gmail_id(&self) -> &str {
        match self {
            Self::Fetched { gmail_id, .. }
            | Self::Gone { gmail_id }
            | Self::Failed { gmail_id, .. } => gmail_id,
        }
    }
}

fn outcome(gmail_id: &str, response: &BatchResponse) -> BatchOutcome {
    if response.is_gone() {
        return BatchOutcome::Gone {
            gmail_id: gmail_id.to_owned(),
        };
    }
    if !response.is_success() {
        return BatchOutcome::Failed {
            gmail_id: gmail_id.to_owned(),
            status: response.status,
            detail: String::from_utf8_lossy(&response.body)
                .chars()
                .take(300)
                .collect(),
        };
    }
    match serde_json::from_slice::<GmailMessage>(&response.body) {
        Ok(message) => {
            let raw = message.raw.as_deref().and_then(decode_base64url);
            match raw {
                Some(raw) => BatchOutcome::Fetched {
                    gmail_id: gmail_id.to_owned(),
                    message: Box::new(message),
                    raw,
                },
                None => BatchOutcome::Failed {
                    gmail_id: gmail_id.to_owned(),
                    status: response.status,
                    detail: "the sub-response carried no decodable `raw`".to_owned(),
                },
            }
        }
        Err(error) => BatchOutcome::Failed {
            gmail_id: gmail_id.to_owned(),
            status: response.status,
            detail: format!("unreadable JSON: {error}"),
        },
    }
}

/// Gmail's base64url, which is sometimes padded and sometimes not, and sometimes
/// carries the line breaks of the original MIME body.
#[must_use]
pub fn decode_base64url(value: &str) -> Option<Vec<u8>> {
    let cleaned: String = value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cleaned.trim_end_matches('='))
        .ok()
        .or_else(|| {
            base64::engine::general_purpose::STANDARD
                .decode(cleaned.trim_end_matches('='))
                .ok()
        })
}

fn encode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use wiremock::{
        matchers::{method, path, path_regex, query_param},
        Mock, MockServer, Request, ResponseTemplate,
    };

    use super::*;
    use crate::gmail::oauth::StaticTokens;

    /// A client pointed at a wiremock server, with retries that do not sleep.
    ///
    /// `crate::gmail::http_client()` rather than `reqwest::Client::new()`: the
    /// crate takes `rustls-no-provider`, so a client built any other way panics
    /// at construction. `no_bare_reqwest_clients` keeps that honest.
    fn client_for(server: &MockServer) -> GmailClient {
        GmailClient::new(
            crate::gmail::http_client().expect("building the shared http client"),
            Endpoints::at(&server.uri()),
            Arc::new(Bucket::new()),
            Arc::new(StaticTokens("ya29.test-token".to_owned())),
        )
        .with_retry_budget(4, 0.001)
    }

    #[tokio::test]
    async fn get_profile_reads_the_history_id_and_sends_the_bearer() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/gmail/v1/users/me/profile"))
            .respond_with(|request: &Request| {
                assert_eq!(
                    request.headers.get("authorization").unwrap(),
                    "Bearer ya29.test-token",
                    "every call must carry the bearer token"
                );
                ResponseTemplate::new(200).set_body_raw(
                    br#"{"emailAddress":"jatinsethi98@gmail.com","messagesTotal":63120,
                         "historyId":"9412771"}"#
                        .to_vec(),
                    "application/json",
                )
            })
            .mount(&server)
            .await;

        let profile = client_for(&server).get_profile().await.unwrap();
        assert_eq!(profile.email_address, "jatinsethi98@gmail.com");
        assert_eq!(profile.history_id.as_deref(), Some("9412771"));
    }

    #[tokio::test]
    async fn labels_list_round_trips() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/gmail/v1/users/me/labels"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(
                    br#"{"labels":[{"id":"INBOX","name":"INBOX","type":"system"},
                              {"id":"Label_12","name":"To Reply","type":"user"}]}"#
                        .to_vec(),
                    "application/json",
                ),
            )
            .mount(&server)
            .await;

        let labels = client_for(&server).list_labels().await.unwrap();
        assert_eq!(labels.len(), 2);
        assert_eq!(labels[1].name.as_deref(), Some("To Reply"));
        assert_eq!(labels[1].kind.as_deref(), Some("user"));
    }

    /// Criterion M2 - and the dev cap is enforced by the client, not trusted to
    /// the caller.
    #[tokio::test]
    async fn list_paginates_and_stops_at_the_cap() {
        let server = MockServer::start().await;
        let calls = Arc::new(AtomicUsize::new(0));

        Mock::given(method("GET"))
            .and(path("/gmail/v1/users/me/messages"))
            .and(query_param("q", "newer_than:30d"))
            .respond_with({
                let calls = Arc::clone(&calls);
                move |_: &Request| {
                    let page = calls.fetch_add(1, Ordering::SeqCst);
                    let ids: Vec<String> = (0..500)
                        .map(|index| {
                            format!(
                                r#"{{"id":"m{}","threadId":"t{}"}}"#,
                                page * 500 + index,
                                page * 500 + index
                            )
                        })
                        .collect();
                    ResponseTemplate::new(200).set_body_raw(
                        format!(
                            r#"{{"messages":[{}],"nextPageToken":"page-{}"}}"#,
                            ids.join(","),
                            page + 1
                        )
                        .into_bytes(),
                        "application/json",
                    )
                }
            })
            .mount(&server)
            .await;

        let ids = client_for(&server)
            .list_message_ids("newer_than:30d", 1200)
            .await
            .unwrap();

        assert_eq!(ids.len(), 1200, "the cap is a hard stop");
        assert_eq!(ids[0].id, "m0");
        assert_eq!(ids[1199].id, "m1199");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "500 + 500 + 200 is three pages, and no fourth"
        );
    }

    /// EDGE (pagination boundary): a server that always returns a token but no
    /// rows must not spin forever.
    #[tokio::test]
    async fn an_empty_page_with_a_token_terminates() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/gmail/v1/users/me/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                br#"{"messages":[],"nextPageToken":"forever"}"#.to_vec(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let ids = client_for(&server)
            .list_message_ids("newer_than:30d", 2000)
            .await
            .unwrap();
        assert!(ids.is_empty());
    }

    #[tokio::test]
    async fn get_raw_and_metadata_hit_the_right_formats() {
        let server = MockServer::start().await;
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(b"From: a@b.com\r\nSubject: Hi\r\n\r\nBody\r\n");

        Mock::given(method("GET"))
            .and(path("/gmail/v1/users/me/messages/18f2"))
            .and(query_param("format", "raw"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(r#"{{"id":"18f2","threadId":"t1","raw":"{raw}","internalDate":"1755335524000"}}"#)
                    .into_bytes(),
                "application/json",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/gmail/v1/users/me/messages/18f2"))
            .and(query_param("format", "metadata"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                br#"{"id":"18f2","threadId":"t1","labelIds":["INBOX","UNREAD"]}"#.to_vec(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let client = client_for(&server);
        let full = client.get_message_raw("18f2").await.unwrap();
        assert!(decode_base64url(full.raw.as_deref().unwrap())
            .unwrap()
            .starts_with(b"From: a@b.com"));

        let meta = client.get_message_metadata("18f2").await.unwrap();
        assert_eq!(meta.label_ids, vec!["INBOX", "UNREAD"]);
        assert!(meta.raw.is_none());
    }

    /// `history.list`: the happy page, the pagination, and - the part that
    /// matters - the `404` that means "your cursor is too old, re-sync".
    #[tokio::test]
    async fn history_pages_and_reports_a_stale_cursor() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/gmail/v1/users/me/history"))
            .and(query_param("startHistoryId", "9412771"))
            .respond_with(|request: &Request| {
                let types: Vec<String> = request
                    .url
                    .query_pairs()
                    .filter(|(key, _)| key == "historyTypes")
                    .map(|(_, value)| value.into_owned())
                    .collect();
                assert_eq!(
                    types,
                    vec![
                        "messageAdded",
                        "messageDeleted",
                        "labelAdded",
                        "labelRemoved"
                    ],
                    "all four history types, or an incremental sync loses events"
                );
                let page = request
                    .url
                    .query_pairs()
                    .find(|(key, _)| key == "pageToken")
                    .map(|(_, value)| value.into_owned());
                let body = if page.is_none() {
                    r#"{"history":[
                         {"id":"9412772","messagesAdded":[{"message":{"id":"m1","threadId":"t1"}}]},
                         {"id":"9412773","labelsRemoved":[
                            {"message":{"id":"m2","threadId":"t2"},"labelIds":["UNREAD"]}]}],
                       "nextPageToken":"page-2","historyId":"9412780"}"#
                } else {
                    r#"{"history":[
                         {"id":"9412779","messagesDeleted":[{"message":{"id":"m3"}}]}],
                       "historyId":"9412780"}"#
                };
                ResponseTemplate::new(200)
                    .set_body_raw(body.as_bytes().to_vec(), "application/json")
            })
            .mount(&server)
            .await;

        let client = client_for(&server);
        let first = client.list_history(9_412_771, None).await.unwrap();
        assert_eq!(first.history.len(), 2);
        assert_eq!(first.next_page_token.as_deref(), Some("page-2"));
        assert_eq!(first.touched_message_ids(), vec!["m1", "m2"]);

        let second = client
            .list_history(9_412_771, Some("page-2"))
            .await
            .unwrap();
        assert!(second.next_page_token.is_none());
        assert_eq!(second.touched_message_ids(), vec!["m3"]);
        assert_eq!(second.history_id.as_deref(), Some("9412780"));

        // A cursor beyond Gmail's ~week of retention.
        let stale = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/gmail/v1/users/me/history"))
            .respond_with(ResponseTemplate::new(404).set_body_raw(
                br#"{"error":{"code":404,"message":"Requested entity was not found."}}"#.to_vec(),
                "application/json",
            ))
            .mount(&stale)
            .await;
        let error = client_for(&stale).list_history(1, None).await.unwrap_err();
        assert!(
            matches!(error, GmailError::NotFound),
            "a stale cursor must be distinguishable from an outage: {error}"
        );

        // EDGE (empty input): a quiet mailbox is an empty page, not an error.
        let quiet = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/gmail/v1/users/me/history"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(br#"{"historyId":"9412771"}"#.to_vec(), "application/json"),
            )
            .mount(&quiet)
            .await;
        let empty = client_for(&quiet)
            .list_history(9_412_771, None)
            .await
            .unwrap();
        assert!(empty.history.is_empty());
        assert!(empty.touched_message_ids().is_empty());
    }

    #[tokio::test]
    async fn attachments_are_fetched_and_decoded() {
        let server = MockServer::start().await;
        let bytes = b"%PDF-1.7 fake";
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        Mock::given(method("GET"))
            .and(path_regex(
                r"^/gmail/v1/users/me/messages/.+/attachments/.+$",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(r#"{{"size":{},"data":"{encoded}"}}"#, bytes.len()).into_bytes(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let got = client_for(&server)
            .get_attachment("18f2", "ANGjdJ_qk9mR0xT2wPmC5nQ")
            .await
            .unwrap();
        assert_eq!(got, bytes);
    }

    /// A message that vanished between list and get.
    #[tokio::test]
    async fn a_404_is_not_found_and_is_not_retried() {
        let server = MockServer::start().await;
        let calls = Arc::new(AtomicUsize::new(0));
        Mock::given(method("GET"))
            .and(path("/gmail/v1/users/me/messages/gone"))
            .respond_with({
                let calls = Arc::clone(&calls);
                move |_: &Request| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    ResponseTemplate::new(404).set_body_raw(
                        br#"{"error":{"code":404,"message":"Not Found"}}"#.to_vec(),
                        "application/json",
                    )
                }
            })
            .mount(&server)
            .await;

        let error = client_for(&server)
            .get_message_raw("gone")
            .await
            .unwrap_err();
        assert!(matches!(error, GmailError::NotFound), "{error}");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a 404 must never be retried"
        );
    }

    /// Criterion L5 - 429 retries, on the backoff schedule.
    #[tokio::test]
    async fn retries_429_then_succeeds() {
        let server = MockServer::start().await;
        let calls = Arc::new(AtomicUsize::new(0));
        Mock::given(method("GET"))
            .and(path("/gmail/v1/users/me/profile"))
            .respond_with({
                let calls = Arc::clone(&calls);
                move |_: &Request| {
                    let seen = calls.fetch_add(1, Ordering::SeqCst);
                    if seen < 2 {
                        ResponseTemplate::new(429)
                            .insert_header("retry-after", "1")
                            .set_body_raw(
                                br#"{"error":{"code":429,"message":"Too Many Requests",
                                     "errors":[{"reason":"rateLimitExceeded"}]}}"#
                                    .to_vec(),
                                "application/json",
                            )
                    } else {
                        ResponseTemplate::new(200).set_body_raw(
                            br#"{"emailAddress":"a@b.com","historyId":"1"}"#.to_vec(),
                            "application/json",
                        )
                    }
                }
            })
            .mount(&server)
            .await;

        let profile = client_for(&server).get_profile().await.unwrap();
        assert_eq!(profile.email_address, "a@b.com");
        assert_eq!(calls.load(Ordering::SeqCst), 3, "two 429s, then the answer");
    }

    /// Criterion L5 - 403 `rateLimitExceeded` retries; a plain 403 does not.
    #[tokio::test]
    async fn a_throttling_403_retries_but_a_permission_403_does_not() {
        let server = MockServer::start().await;
        let throttled = Arc::new(AtomicUsize::new(0));
        Mock::given(method("GET"))
            .and(path("/gmail/v1/users/me/profile"))
            .respond_with({
                let throttled = Arc::clone(&throttled);
                move |_: &Request| {
                    let seen = throttled.fetch_add(1, Ordering::SeqCst);
                    if seen == 0 {
                        ResponseTemplate::new(403).set_body_raw(
                            br#"{"error":{"code":403,"errors":[{"reason":"rateLimitExceeded"}]}}"#
                                .to_vec(),
                            "application/json",
                        )
                    } else {
                        ResponseTemplate::new(200).set_body_raw(
                            br#"{"emailAddress":"a@b.com"}"#.to_vec(),
                            "application/json",
                        )
                    }
                }
            })
            .mount(&server)
            .await;

        assert!(client_for(&server).get_profile().await.is_ok());
        assert_eq!(throttled.load(Ordering::SeqCst), 2);

        let denied = MockServer::start().await;
        let calls = Arc::new(AtomicUsize::new(0));
        Mock::given(method("GET"))
            .and(path("/gmail/v1/users/me/profile"))
            .respond_with({
                let calls = Arc::clone(&calls);
                move |_: &Request| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    ResponseTemplate::new(403).set_body_raw(
                        br#"{"error":{"code":403,"message":"Insufficient Permission",
                             "errors":[{"reason":"insufficientPermissions"}]}}"#
                            .to_vec(),
                        "application/json",
                    )
                }
            })
            .mount(&denied)
            .await;

        let error = client_for(&denied).get_profile().await.unwrap_err();
        assert!(matches!(error, GmailError::Upstream(_)), "{error}");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a permission failure must not be retried"
        );
    }

    /// A 401 refreshes once; a second 401 is a dead credential rather than a
    /// retry loop.
    #[tokio::test]
    async fn a_401_refreshes_once_then_gives_up() {
        let server = MockServer::start().await;
        let calls = Arc::new(AtomicUsize::new(0));
        Mock::given(method("GET"))
            .and(path("/gmail/v1/users/me/profile"))
            .respond_with({
                let calls = Arc::clone(&calls);
                move |_: &Request| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    ResponseTemplate::new(401).set_body_raw(
                        br#"{"error":{"code":401,"message":"Invalid Credentials"}}"#.to_vec(),
                        "application/json",
                    )
                }
            })
            .mount(&server)
            .await;

        let error = client_for(&server).get_profile().await.unwrap_err();
        assert!(matches!(error, GmailError::NeedsReauth), "{error}");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// Criterion M1 + M5, end to end over HTTP: a real multipart batch where one
    /// sub-request 404s.
    #[tokio::test]
    async fn a_real_batch_returns_the_other_44_when_one_is_gone() {
        let server = MockServer::start().await;
        let ids: Vec<String> = (0..45).map(|index| format!("msg{index}")).collect();

        Mock::given(method("POST"))
            .and(path("/batch/gmail/v1"))
            .respond_with(|request: &Request| {
                let body = String::from_utf8_lossy(&request.body).into_owned();
                assert!(
                    request
                        .headers
                        .get("content-type")
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .starts_with("multipart/mixed; boundary="),
                    "the batch must be a real multipart request"
                );
                assert_eq!(body.matches("Content-ID: <item-").count(), 45);

                // Answer in reverse order, with item-7 gone, to prove the
                // correlation is by Content-ID rather than by position.
                let mut out = String::new();
                for index in (0..45).rev() {
                    let (status, payload) = if index == 7 {
                        (404, r#"{"error":{"code":404,"message":"Not Found"}}"#.to_owned())
                    } else {
                        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
                            format!("From: s{index}@example.com\r\nSubject: S{index}\r\n\r\nBody {index}\r\n")
                                .as_bytes(),
                        );
                        (
                            200,
                            format!(r#"{{"id":"msg{index}","threadId":"t{index}","raw":"{raw}"}}"#),
                        )
                    };
                    out.push_str(&format!(
                        "--bnd\r\nContent-Type: application/http\r\n\
                         Content-ID: <response-item-{index}>\r\n\r\n\
                         HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\n\r\n{payload}\r\n"
                    ));
                }
                out.push_str("--bnd--\r\n");
                ResponseTemplate::new(200)
                    .set_body_raw(out.into_bytes(), "multipart/mixed; boundary=bnd")
            })
            .mount(&server)
            .await;

        let outcomes = client_for(&server).batch_get_raw(&ids).await.unwrap();
        assert_eq!(outcomes.len(), 45);

        let fetched: Vec<&BatchOutcome> = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, BatchOutcome::Fetched { .. }))
            .collect();
        assert_eq!(fetched.len(), 44, "one 404 must not cost us the other 44");
        assert!(matches!(&outcomes[7], BatchOutcome::Gone { gmail_id } if gmail_id == "msg7"));

        // Every fetched outcome carries *its own* message, not its neighbour's.
        for (index, outcome) in outcomes.iter().enumerate() {
            if let BatchOutcome::Fetched { gmail_id, raw, .. } = outcome {
                assert_eq!(gmail_id, &format!("msg{index}"));
                let text = String::from_utf8_lossy(raw);
                assert!(
                    text.contains(&format!("Subject: S{index}")),
                    "msg{index} got the wrong body: {text}"
                );
            }
        }
    }

    /// Criterion M7, at the client level.
    #[tokio::test]
    async fn an_empty_batch_makes_no_request() {
        let server = MockServer::start().await;
        // No mocks are mounted: any request at all would 404 and fail the test.
        let outcomes = client_for(&server).batch_get_raw(&[]).await.unwrap();
        assert!(outcomes.is_empty());
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_oversized_batch_is_refused_locally() {
        let server = MockServer::start().await;
        let ids: Vec<String> = (0..46).map(|i| format!("m{i}")).collect();
        let error = client_for(&server).batch_get_raw(&ids).await.unwrap_err();
        assert!(error.to_string().contains("at most 45"), "{error}");
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_non_multipart_batch_response_is_an_upstream_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/batch/gmail/v1"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(b"<html>nope</html>".to_vec(), "text/html"),
            )
            .mount(&server)
            .await;

        let error = client_for(&server)
            .batch_get_raw(&["m1".to_owned()])
            .await
            .unwrap_err();
        assert!(matches!(error, GmailError::Upstream(_)), "{error}");
    }

    #[test]
    fn base64url_decoding_tolerates_padding_and_newlines() {
        let plain = b"hello world";
        let unpadded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(plain);
        let padded = base64::engine::general_purpose::STANDARD.encode(plain);
        assert_eq!(decode_base64url(&unpadded).unwrap(), plain);
        assert_eq!(decode_base64url(&padded).unwrap(), plain);
        assert_eq!(decode_base64url("aGVsbG8g\r\nd29ybGQ").unwrap(), plain);
        assert_eq!(decode_base64url(""), Some(Vec::new()));
        assert_eq!(decode_base64url("!!!!"), None);
    }
}
