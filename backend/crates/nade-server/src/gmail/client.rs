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
        Attachment, GmailMessage, GmailThread, HistoryList, Label, LabelsList, MessageRef,
        MessagesList, Profile, WatchRegistration,
    },
};

/// The widest batch this client will send.
///
/// **Recorded deviation from PLAN.md §Gmail sync, forced by the first live
/// sync.** PLAN.md pinned 45 because 45 x 5 = 225 quota units sits inside the
/// 250 units/second ceiling. That arithmetic is correct and it is about the
/// wrong limit: Google expands a `multipart/mixed` batch and runs the
/// sub-requests **concurrently**, so a 45-wide batch is 45 simultaneous
/// requests for one user, and Gmail answered 91 of them with
/// `429 rateLimitExceeded: "Too many concurrent requests for user."` while the
/// unit budget still read 225 of 250.
///
/// [`quota::MAX_CONCURRENT_SUBREQUESTS`] carries the reasoning; the cap lives
/// here because this is the only place that can enforce it.
pub const MAX_BATCH: usize = quota::MAX_CONCURRENT_SUBREQUESTS;

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
    /// Returns the ids **and whether Gmail had more to give**.
    ///
    /// The second half matters to the reconciliation sweep: `listed == cap` is
    /// not proof of truncation (a mailbox of exactly the cap with no next page
    /// is complete), and treating it as such narrows the sweep's floor and
    /// leaves genuinely deleted rows behind.
    pub async fn list_message_ids(
        &self,
        query: &str,
        cap: usize,
    ) -> Result<(Vec<MessageRef>, bool), GmailError> {
        let mut out: Vec<MessageRef> = Vec::new();
        let mut page_token: Option<String> = None;

        while out.len() < cap {
            let want = cap - out.len();
            let page = self
                .list_message_page(query, page_token.as_deref(), want)
                .await?;
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

        // Truncated means "Gmail still had more when we stopped", which is a
        // page token surviving the loop - not `out.len() == cap`.
        let truncated = out.len() > cap || (page_token.is_some() && out.len() >= cap);
        out.truncate(cap);
        Ok((out, truncated))
    }

    /// **One** page of `users.messages.list`, with Gmail's own `pageToken`.
    ///
    /// The sync wants every id and pages internally; `GET /v1/search` wants
    /// exactly one page, because Gmail's page boundary *is* the API's cursor
    /// (`docs/SEARCH.md`). Sharing the request-building keeps the two from
    /// drifting on `maxResults` clamping or parameter encoding.
    ///
    /// # Errors
    /// Returns [`GmailError`] on an upstream failure. Note that a **malformed
    /// `q` is not one**: Gmail answers it with an empty `200`, which is why
    /// every caller validates the query first
    /// ([`crate::search::query::validate`]).
    pub async fn list_message_page(
        &self,
        query: &str,
        page_token: Option<&str>,
        max_results: usize,
    ) -> Result<MessagesList, GmailError> {
        let want = max_results.clamp(1, LIST_PAGE);
        let mut path = format!(
            "/gmail/v1/users/me/messages?maxResults={want}&q={}",
            encode(query)
        );
        if let Some(token) = page_token {
            path.push_str(&format!("&pageToken={}", encode(token)));
        }
        self.get_json(&path, quota::cost::MESSAGES_LIST).await
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
        self.get_message(id, "raw").await
    }

    /// One `users.messages.get`, in whichever format the caller needs. The three
    /// wrappers exist for their doc comments, not their bodies.
    async fn get_message(&self, id: &str, format: &str) -> Result<GmailMessage, GmailError> {
        self.get_json(
            &format!("/gmail/v1/users/me/messages/{}?format={format}", encode(id)),
            quota::cost::MESSAGES_GET,
        )
        .await
    }

    /// `users.messages.get?format=metadata` - headers only, no body.
    ///
    /// # Errors
    /// [`GmailError::NotFound`] when the message is gone.
    pub async fn get_message_metadata(&self, id: &str) -> Result<GmailMessage, GmailError> {
        self.get_message(id, "metadata").await
    }

    /// `users.messages.get?format=full` - the part tree, which is the only place
    /// Gmail's `attachmentId` appears. `format=raw` does not carry it at all.
    ///
    /// # Errors
    /// [`GmailError::NotFound`] when the message is gone.
    pub async fn get_message_full(&self, id: &str) -> Result<GmailMessage, GmailError> {
        self.get_message(id, "full").await
    }

    /// `users.threads.get?format=minimal` - the thread's whole message list,
    /// ids and labels only.
    ///
    /// This is the only call that answers "how much of this conversation is
    /// there?". `messages.list` never says how big a thread is, so without it a
    /// windowed cache cannot tell a complete thread from a fragment of one.
    ///
    /// # Errors
    /// [`GmailError::NotFound`] when the thread is gone.
    pub async fn get_thread_minimal(&self, thread_id: &str) -> Result<GmailThread, GmailError> {
        self.get_json(
            &format!(
                "/gmail/v1/users/me/threads/{}?format=minimal",
                encode(thread_id)
            ),
            quota::cost::THREADS_GET,
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

    /// `users.watch`. Registers the mailbox against `topic` for seven days.
    ///
    /// Calling it again **replaces** the registration rather than adding one,
    /// which is exactly what the daily renewal wants: there is no "unregister
    /// then register" window in which a notification could be lost.
    ///
    /// An empty `label_ids` means "every change". That is deliberate - the push
    /// is only a trigger, and the walk that follows reads all four history
    /// types regardless, so filtering here would only mean archive and label
    /// events waited for the polling fallback.
    ///
    /// # Errors
    /// [`GmailError::Upstream`] if the topic is malformed or the Publisher
    /// grant is missing; [`GmailError::NeedsReauth`] on a dead credential.
    pub async fn watch(
        &self,
        topic: &str,
        label_ids: &[String],
    ) -> Result<WatchRegistration, GmailError> {
        let mut body = serde_json::json!({ "topicName": topic });
        if !label_ids.is_empty() {
            body["labelIds"] = serde_json::json!(label_ids);
            body["labelFilterBehavior"] = serde_json::json!("include");
        }
        self.post_json("/gmail/v1/users/me/watch", &body, quota::cost::WATCH)
            .await
    }

    /// `users.stop`. Ends the push registration.
    ///
    /// Answers **204 with no body**, so it must not go through
    /// [`Self::post_json`] - deserialising nothing is a hard error, and it
    /// would turn a successful stop into an upstream failure.
    ///
    /// Stopping a mailbox that was never watched is a success, not an error:
    /// the postcondition ("no registration") already holds.
    ///
    /// # Errors
    /// Returns [`GmailError`] on an upstream failure.
    pub async fn stop_watch(&self) -> Result<(), GmailError> {
        self.post_discard("/gmail/v1/users/me/stop", quota::cost::STOP)
            .await
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
                        // We know nothing about this message, and "we know
                        // nothing" is never permission to forget it.
                        transient: true,
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

    async fn post_json<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
        cost: u32,
    ) -> Result<T, GmailError> {
        let url = format!("{}{path}", self.endpoints.api_base);
        let payload = serde_json::to_vec(body)
            .map_err(|error| GmailError::Upstream(format!("cannot serialise {path}: {error}")))?;
        let (_, response) = self
            .send(
                reqwest::Method::POST,
                &url,
                Some(("application/json".to_owned(), payload)),
                cost,
            )
            .await?;
        serde_json::from_slice(&response).map_err(|error| {
            GmailError::Upstream(format!(
                "gmail returned unreadable JSON for {path}: {error}"
            ))
        })
    }

    /// A POST whose response body is discarded. `users.stop` answers `204` with
    /// nothing at all, and an empty body is not valid JSON.
    async fn post_discard(&self, path: &str, cost: u32) -> Result<(), GmailError> {
        let url = format!("{}{path}", self.endpoints.api_base);
        self.send(reqwest::Method::POST, &url, None, cost).await?;
        Ok(())
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
            // Both quota axes, released the moment the response is in hand:
            // holding a concurrency slot through a 60 s backoff would starve
            // every other caller for no benefit. `slot` is dropped explicitly
            // below, before any sleep.
            let slot = self.quota.enter(cost).await;
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
                    // Off the wire: give the concurrency slot back before we
                    // decide anything, and certainly before we sleep on it.
                    drop(slot);

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
                        // A failed invalidation is the **database** blinking,
                        // never evidence that the credential is dead. Swallowing
                        // it would re-send the same expired token, burn the one
                        // auth retry above, and end in `needs_reauth` - telling
                        // the user to reconnect Gmail because Postgres was
                        // briefly away. Surface it as the retryable upstream
                        // failure it is instead.
                        if let Err(error) = self.tokens.invalidate().await {
                            return Err(GmailError::Upstream(format!(
                                "could not invalidate the cached access token, so the refresh \
                                 would have re-sent the dead one: {error}"
                            )));
                        }
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
                    drop(slot);
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

    /// Delegates to [`types::is_transient`], which is also what the sync loop
    /// consults for a batch sub-response. One classifier, so the two can never
    /// disagree about what a `429` means.
    fn is_retryable(&self, status: reqwest::StatusCode, body: &[u8]) -> bool {
        super::types::is_transient(status.as_u16(), body)
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
        /// The sub-response body, truncated for the audit log.
        detail: String,
        /// Whether asking again could ever help, decided **here** rather than by
        /// the caller.
        ///
        /// The caller only has `detail`, which is cut to 300 characters for the
        /// audit row - and a `403` is classified by reading Google's error
        /// envelope out of the body. A truncated envelope does not parse, so a
        /// throttling `403` with a long body would be read as a permanent
        /// denial and the message dropped. Classifying where the whole body is
        /// still in hand is the only place that cannot happen.
        transient: bool,
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
            transient: super::types::is_transient(response.status, &response.body),
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
                    // A `200` with an unusable body is Gmail misbehaving, not
                    // the message being unfetchable.
                    transient: true,
                },
            }
        }
        Err(error) => BatchOutcome::Failed {
            gmail_id: gmail_id.to_owned(),
            status: response.status,
            detail: format!("unreadable JSON: {error}"),
            transient: true,
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

    /// Criterion S1 - `users.watch` registers the topic, and the expiry comes
    /// back as epoch **milliseconds in a string**.
    #[tokio::test]
    async fn watch_registers_the_topic_and_parses_a_millisecond_expiry() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/gmail/v1/users/me/watch"))
            .respond_with(|request: &Request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(
                    body["topicName"], "projects/p/topics/gmail-events",
                    "the topic is the whole point of the call"
                );
                assert!(
                    body.get("labelIds").is_none(),
                    "an empty label list must not be sent at all - Gmail reads a \
                     present-but-empty list as `include nothing`"
                );
                ResponseTemplate::new(200).set_body_raw(
                    br#"{"historyId":"9412771","expiration":"1755648000000"}"#.to_vec(),
                    "application/json",
                )
            })
            .mount(&server)
            .await;

        let registration = client_for(&server)
            .watch("projects/p/topics/gmail-events", &[])
            .await
            .unwrap();

        assert_eq!(registration.history(), Some(9_412_771));
        // 1755648000000 ms = 2025-08-20T00:00:00Z. Read as *seconds* this would
        // land in the year 57'600 and every renewal check would think the
        // registration was fine forever.
        assert_eq!(
            registration.expires_at().unwrap().to_rfc3339(),
            "2025-08-20T00:00:00+00:00"
        );
    }

    /// A non-empty label filter is sent with its behaviour, or Gmail ignores it.
    #[tokio::test]
    async fn a_label_filtered_watch_sends_the_behaviour_too() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/gmail/v1/users/me/watch"))
            .respond_with(|request: &Request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(body["labelIds"], serde_json::json!(["INBOX"]));
                assert_eq!(body["labelFilterBehavior"], "include");
                ResponseTemplate::new(200).set_body_raw(
                    br#"{"historyId":"1","expiration":"1755648000000"}"#.to_vec(),
                    "application/json",
                )
            })
            .mount(&server)
            .await;

        client_for(&server)
            .watch("projects/p/topics/gmail-events", &["INBOX".to_owned()])
            .await
            .unwrap();
    }

    /// Criterion S3/S4 - `users.stop` answers `204` with **no body**, and an
    /// empty body is not valid JSON. Routed through `post_json` this fails.
    ///
    /// This also covers "stopping a mailbox that was never watched". Gmail
    /// answers `204` either way, so nothing at this layer can tell the two
    /// apart; a second test with the same mock would have looked like coverage
    /// without adding any. The state transition belongs to the simulator, which
    /// actually models the registration.
    #[tokio::test]
    async fn stop_watch_accepts_a_204_with_no_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/gmail/v1/users/me/stop"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        client_for(&server).stop_watch().await.unwrap();
    }

    /// The cursor rule, at the type that carries the trap.
    ///
    /// `history_id` is the mailbox's current id and is repeated on every page;
    /// only `max_record_id` describes what this page actually covered.
    #[test]
    fn the_page_cursor_is_the_last_record_id_and_never_the_top_level_history_id() {
        let page: HistoryList = serde_json::from_str(
            r#"{"history":[{"id":"101"},{"id":"102"}],
                "nextPageToken":"tok","historyId":"999"}"#,
        )
        .unwrap();

        assert_eq!(page.max_record_id(), Some(102));
        assert_eq!(
            page.history_id.as_deref(),
            Some("999"),
            "the trap is still on the type; the helper is what avoids it"
        );

        // An empty page has no cursor at all - it must not move anything.
        let empty: HistoryList = serde_json::from_str(r#"{"historyId":"999"}"#).unwrap();
        assert_eq!(empty.max_record_id(), None);

        // Gmail does not promise the records are sorted, so take the maximum
        // rather than the last element.
        let unsorted: HistoryList =
            serde_json::from_str(r#"{"history":[{"id":"7"},{"id":"5"}]}"#).unwrap();
        assert_eq!(unsorted.max_record_id(), Some(7));
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

        let (ids, truncated) = ids;
        assert_eq!(ids.len(), 1200, "the cap is a hard stop");
        assert_eq!(ids[0].id, "m0");
        assert_eq!(ids[1199].id, "m1199");
        assert!(
            truncated,
            "stopping at the cap with a page token still in hand is truncation"
        );
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
        let (ids, truncated) = ids;
        assert!(ids.is_empty());
        assert!(!truncated, "an empty window is complete, not truncated");
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

    /// A transient **database** error must not be laundered into a permanent
    /// credential failure.
    ///
    /// `AccessTokens::invalidate` used to return `()`. When the `gmail_tokens`
    /// write failed, the 401 arm could not tell: it re-sent the same dead
    /// token, the second 401 burned the one auth retry, and the caller got
    /// `needs_reauth` - so the user was told to reconnect Gmail because
    /// Postgres blinked for a second. It now returns `Result`, and a failure
    /// surfaces as the retryable upstream error it is.
    #[tokio::test]
    async fn a_failed_invalidation_is_upstream_not_needs_reauth() {
        /// A token source whose store is unreachable.
        #[derive(Debug)]
        struct DeadStore;

        #[async_trait::async_trait]
        impl crate::gmail::oauth::AccessTokens for DeadStore {
            async fn access_token(&self) -> Result<String, crate::gmail::oauth::TokenError> {
                Ok("ya29.expired".to_owned())
            }
            async fn invalidate(&self) -> Result<(), crate::gmail::oauth::TokenError> {
                Err(crate::gmail::oauth::TokenError::Other(
                    "pool timed out while connecting".to_owned(),
                ))
            }
        }

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

        let client = GmailClient::new(
            crate::gmail::http_client().unwrap(),
            Endpoints::at(&server.uri()),
            Arc::new(Bucket::new()),
            Arc::new(DeadStore),
        )
        .with_retry_budget(4, 0.001);

        let error = client.get_profile().await.unwrap_err();
        match &error {
            GmailError::Upstream(detail) => assert!(
                detail.contains("invalidate"),
                "the error must name what actually failed: {detail}"
            ),
            other => panic!(
                "a database failure must be a retryable upstream error, not a dead \
                 credential: {other}"
            ),
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the dead token must not be re-sent, because that is what burned the auth retry"
        );
    }

    /// Criterion M1 + M5, end to end over HTTP: a real multipart batch where one
    /// sub-request 404s.
    #[tokio::test]
    async fn a_real_batch_returns_the_others_when_one_is_gone() {
        let server = MockServer::start().await;
        let ids: Vec<String> = (0..MAX_BATCH).map(|index| format!("msg{index}")).collect();

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
                assert_eq!(body.matches("Content-ID: <item-").count(), MAX_BATCH);

                // Answer in reverse order, with item-7 gone, to prove the
                // correlation is by Content-ID rather than by position.
                let mut out = String::new();
                for index in (0..MAX_BATCH).rev() {
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
        assert_eq!(outcomes.len(), MAX_BATCH);

        let fetched: Vec<&BatchOutcome> = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, BatchOutcome::Fetched { .. }))
            .collect();
        assert_eq!(
            fetched.len(),
            MAX_BATCH - 1,
            "one 404 must not cost us the rest of the batch"
        );
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
    /// Retryability is decided from the **whole** sub-response body, not from
    /// the 300-character `detail` the audit log gets.
    ///
    /// A `403` is the only status classified by reading Google's error
    /// envelope, and Google pads that envelope with a `details` array that runs
    /// well past 300 characters. Classify from `detail` and the JSON is cut
    /// mid-object, the envelope fails to parse, a throttle reads as a permanent
    /// denial, and the message is dropped - the same ending as the live bug, by
    /// a different route.
    #[tokio::test]
    async fn a_throttling_403_is_classified_from_the_whole_body_not_the_audit_excerpt() {
        let server = MockServer::start().await;
        let padding = "x".repeat(600);
        let long_403 = format!(
            r#"{{"error":{{"code":403,"message":"Rate Limit Exceeded","errors":[
                 {{"domain":"usageLimits","reason":"rateLimitExceeded",
                   "message":"Rate Limit Exceeded","extendedHelp":"{padding}"}}],
                 "status":"PERMISSION_DENIED"}}}}"#
        );
        assert!(
            long_403.chars().count() > 300,
            "the body must exceed the audit excerpt or this test proves nothing"
        );

        let body = format!(
            "--bnd\r\nContent-Type: application/http\r\nContent-ID: <response-item-0>\r\n\r\n\
             HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\n\r\n{long_403}\r\n\
             --bnd--\r\n"
        );
        Mock::given(method("POST"))
            .and(path("/batch/gmail/v1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(body.into_bytes(), "multipart/mixed; boundary=bnd"),
            )
            .mount(&server)
            .await;

        let outcomes = client_for(&server)
            .batch_get_raw(&["m1".to_owned()])
            .await
            .unwrap();
        let BatchOutcome::Failed {
            transient, detail, ..
        } = &outcomes[0]
        else {
            panic!("expected a failed sub-response, got {:?}", outcomes[0]);
        };
        assert!(
            transient,
            "a throttling 403 must be retried; classifying it from the excerpt loses the message"
        );
        assert_eq!(
            detail.chars().count(),
            300,
            "the audit excerpt is still capped, and still not what the decision reads"
        );
        assert!(
            !crate::gmail::types::is_transient(403, detail.as_bytes()),
            "the excerpt alone really does misclassify - which is why it is not used"
        );
    }

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
        let ids: Vec<String> = (0..MAX_BATCH + 1).map(|i| format!("m{i}")).collect();
        let error = client_for(&server).batch_get_raw(&ids).await.unwrap_err();
        assert!(
            error.to_string().contains(&format!("at most {MAX_BATCH}")),
            "{error}"
        );
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
