//! The simulator: one mailbox, one router, one implementation.
//!
//! [`Simulator::handle`] is the only place a response is ever produced. The
//! in-process transport calls it; the HTTP server calls it; a batch sub-request
//! calls it. There is no second code path to fall out of step.

use std::{
    sync::{Mutex, MutexGuard},
    time::Duration,
};

use serde_json::{json, Value};

use crate::{
    api::{parse_query, GmailApi, Method, SimRequest, SimResponse},
    batch::{self, BatchOrder},
    clock::TestClock,
    error::{ApiError, Result},
    fault::{cost, Fault, FaultRule, FaultTable, Quota},
    history::{HistoryRecord, HistoryType},
    ids::parse_attachment_id,
    label::LabelType,
    mailbox::Mailbox,
    message::MessageSpec,
    query::Query,
    render::{self, fingerprint, Format, HistoryCursor, PageCursor, ResultSizeEstimate},
};

/// How the simulator treats the `Authorization` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthMode {
    /// Every call must carry the current access token. The default, because it
    /// is what Gmail does and because the refresh path is only reachable from
    /// here.
    #[default]
    Enforced,
    /// Ignore authorization entirely, for tests that are about something else.
    Off,
}

/// Knobs that do not belong to the mailbox itself.
#[derive(Debug, Clone)]
pub struct Config {
    /// The mailbox's address. `users/me` and `users/<this>` both route to it.
    pub email_address: String,
    /// Whether the bearer token is checked.
    pub auth: AuthMode,
    /// The order batch sub-responses come back in.
    pub batch_order: BatchOrder,
    /// How `resultSizeEstimate` is computed. Defaults to the saturating shape,
    /// which is never equal to the number of rows returned.
    pub result_size_estimate: ResultSizeEstimate,
    /// How long a `users.watch` registration lasts. Gmail's is seven days.
    pub watch_ttl_ms: i64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            email_address: "me@example.com".to_owned(),
            auth: AuthMode::Enforced,
            batch_order: BatchOrder::default(),
            result_size_estimate: ResultSizeEstimate::default(),
            watch_ttl_ms: 7 * crate::clock::DAY_MS,
        }
    }
}

/// Gmail's `maxResults` default and ceiling for `messages.list` and
/// `threads.list`.
const LIST_DEFAULT_MAX: usize = 100;
const LIST_CEILING: usize = 500;
/// …and for `history.list`, whose default is 100 and ceiling 500.
const HISTORY_DEFAULT_MAX: usize = 100;
const HISTORY_CEILING: usize = 500;

/// A live `users.watch` registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watch {
    /// The Pub/Sub topic.
    pub topic_name: String,
    /// Labels the watch is restricted to, if any.
    pub label_ids: Vec<String>,
    /// When it lapses, in epoch milliseconds.
    pub expiration_ms: i64,
}

/// OAuth state, so the refresh and `needs_reauth` paths are reachable.
#[derive(Debug, Clone)]
pub struct Auth {
    /// The token every call must present.
    pub access_token: String,
    /// Whether that token is currently accepted.
    pub access_token_expired: bool,
    /// The refresh token the token endpoint accepts.
    pub refresh_token: String,
    /// When false, a refresh returns `invalid_grant` — the `needs_reauth` path.
    pub refresh_valid: bool,
    /// Whether a successful refresh also rotates the refresh token, which
    /// Google does for some clients and which a client must persist.
    pub rotate_refresh_token: bool,
    issued: u64,
}

impl Default for Auth {
    fn default() -> Self {
        Self {
            access_token: "ya29.sim-access-0".to_owned(),
            access_token_expired: false,
            refresh_token: "1//sim-refresh-0".to_owned(),
            refresh_valid: true,
            rotate_refresh_token: true,
            issued: 0,
        }
    }
}

/// One entry of the call log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallRecord {
    /// The method.
    pub method: Method,
    /// Path and query, as received.
    pub target: String,
    /// The status returned.
    pub status: u16,
    /// Whether this was a batch sub-request.
    pub in_batch: bool,
}

struct Inner {
    mailbox: Mailbox,
    faults: FaultTable,
    quota: Quota,
    auth: Auth,
    watch: Option<Watch>,
    log: Vec<CallRecord>,
    batch_seq: u64,
}

/// A whole simulated Gmail.
///
/// ```
/// use nade_gmail_sim::{MessageSpec, Simulator};
///
/// let sim = Simulator::new();
/// sim.insert_message(&MessageSpec::new().subject("hello")).unwrap();
///
/// let response = sim.handle(&sim.authorized("/gmail/v1/users/me/profile"));
/// assert_eq!(response.status, 200);
/// assert_eq!(response.json_body()["messagesTotal"], 1);
/// ```
pub struct Simulator {
    clock: TestClock,
    config: Config,
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for Simulator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Simulator")
            .field("clock", &self.clock)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Default for Simulator {
    fn default() -> Self {
        Self::new()
    }
}

impl Simulator {
    /// A simulator with an empty mailbox, a clock at
    /// [`DEFAULT_NOW_MS`](crate::clock::DEFAULT_NOW_MS), quota off and auth on.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(Config::default())
    }

    /// A simulator with explicit configuration.
    #[must_use]
    pub fn with_config(config: Config) -> Self {
        let clock = TestClock::new();
        let now = clock.now_ms();
        Self {
            inner: Mutex::new(Inner {
                mailbox: Mailbox::new(config.email_address.clone()),
                faults: FaultTable::default(),
                quota: Quota::new(now),
                auth: Auth::default(),
                watch: None,
                log: Vec::new(),
                batch_seq: 0,
            }),
            clock,
            config,
        }
    }

    /// Share this simulator's clock. Cloning it gives a handle a test can
    /// advance.
    #[must_use]
    pub fn clock(&self) -> TestClock {
        self.clock.clone()
    }

    /// The configuration this simulator was built with.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A poisoned lock means a test panicked mid-mutation. Recovering is
        // right here: the panic is the failure the test will report, and a
        // second panic from the lock would bury it.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    // -- driving the world -------------------------------------------------

    /// Read the mailbox.
    pub fn mailbox<R>(&self, read: impl FnOnce(&Mailbox) -> R) -> R {
        read(&self.lock().mailbox)
    }

    /// Change the mailbox directly, for anything the convenience methods do not
    /// cover.
    pub fn mailbox_mut<R>(&self, change: impl FnOnce(&mut Mailbox) -> R) -> R {
        change(&mut self.lock().mailbox)
    }

    /// Insert a message at the current simulated time.
    ///
    /// # Errors
    /// Propagates [`crate::SimError`] from the mailbox.
    pub fn insert_message(&self, spec: &MessageSpec) -> Result<String> {
        let now = self.clock.now_ms();
        self.lock().mailbox.insert_message(spec, now)
    }

    /// Permanently delete a message.
    ///
    /// # Errors
    /// Propagates [`crate::SimError`] from the mailbox.
    pub fn delete_message(&self, id: &str) -> Result<Option<u64>> {
        self.lock().mailbox.delete_message(id)
    }

    /// Move a message to Trash.
    ///
    /// # Errors
    /// Propagates [`crate::SimError`] from the mailbox.
    pub fn trash_message(&self, id: &str) -> Result<Option<u64>> {
        self.lock().mailbox.trash_message(id)
    }

    /// Add a label.
    ///
    /// # Errors
    /// Propagates [`crate::SimError`] from the mailbox.
    pub fn add_label(&self, id: &str, label: &str) -> Result<Option<u64>> {
        self.lock().mailbox.add_label(id, label)
    }

    /// Remove a label.
    ///
    /// # Errors
    /// Propagates [`crate::SimError`] from the mailbox.
    pub fn remove_label(&self, id: &str, label: &str) -> Result<Option<u64>> {
        self.lock().mailbox.remove_label(id, label)
    }

    /// Drop `UNREAD`.
    ///
    /// # Errors
    /// Propagates [`crate::SimError`] from the mailbox.
    pub fn mark_read(&self, id: &str) -> Result<Option<u64>> {
        self.lock().mailbox.mark_read(id)
    }

    /// The mailbox's current `historyId`.
    #[must_use]
    pub fn history_id(&self) -> u64 {
        self.lock().mailbox.history_id()
    }

    // -- faults, quota, auth ----------------------------------------------

    /// Arm a fault.
    pub fn inject(&self, rule: FaultRule) {
        self.lock().faults.push(rule);
    }

    /// Disarm every fault.
    pub fn clear_faults(&self) {
        self.lock().faults.clear();
    }

    /// Read or change the quota bucket. Disabled by default.
    pub fn quota_mut<R>(&self, change: impl FnOnce(&mut Quota) -> R) -> R {
        change(&mut self.lock().quota)
    }

    /// Turn the 250-units-per-second bucket on.
    pub fn enable_quota(&self) {
        self.lock().quota.enabled = true;
    }

    /// The token every call must currently present.
    #[must_use]
    pub fn access_token(&self) -> String {
        self.lock().auth.access_token.clone()
    }

    /// The refresh token the token endpoint accepts.
    #[must_use]
    pub fn refresh_token(&self) -> String {
        self.lock().auth.refresh_token.clone()
    }

    /// Read or change the OAuth state.
    pub fn auth_mut<R>(&self, change: impl FnOnce(&mut Auth) -> R) -> R {
        change(&mut self.lock().auth)
    }

    /// Expire the access token, so every call 401s until the client refreshes.
    pub fn expire_access_token(&self) {
        self.lock().auth.access_token_expired = true;
    }

    /// Make the next refresh fail with `invalid_grant` — the `needs_reauth`
    /// path.
    pub fn revoke_refresh_token(&self) {
        self.lock().auth.refresh_valid = false;
    }

    /// The live `users.watch` registration, if any.
    #[must_use]
    pub fn watch(&self) -> Option<Watch> {
        self.lock().watch.clone()
    }

    /// Every call the simulator has answered, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<CallRecord> {
        self.lock().log.clone()
    }

    /// Forget the call log.
    pub fn clear_calls(&self) {
        self.lock().log.clear();
    }

    // -- requests ----------------------------------------------------------

    /// A `GET` carrying the current access token — the common case in a test
    /// that is not about authorization.
    #[must_use]
    pub fn authorized(&self, path_and_query: &str) -> SimRequest {
        SimRequest::get(path_and_query).bearer(&self.access_token())
    }

    /// A `POST` carrying the current access token.
    #[must_use]
    pub fn authorized_post(&self, path_and_query: &str, body: impl Into<Vec<u8>>) -> SimRequest {
        SimRequest::post(path_and_query, body).bearer(&self.access_token())
    }

    /// Answer one request. The only place a response is produced.
    #[must_use]
    pub fn handle(&self, request: &SimRequest) -> SimResponse {
        let response = self.dispatch(request, false);
        let mut inner = self.lock();
        inner.log.push(CallRecord {
            method: request.method,
            target: request.target(),
            status: response.status,
            in_batch: false,
        });
        response
    }

    /// Answer one request that arrived inside a batch.
    fn dispatch(&self, request: &SimRequest, in_batch: bool) -> SimResponse {
        let now = self.clock.now_ms();

        // 1. Faults, before anything else: they stand in for the network and the
        //    front end, which do not care whether the token is any good.
        let verdict = self.lock().faults.verdict(request, in_batch);
        if let Some(fault) = verdict.fault {
            let mut response = self.apply_fault(&fault);
            response.latency = verdict.delay;
            return response;
        }

        // 2. The OAuth token endpoint is not a Gmail API call: no bearer, no
        //    quota.
        if is_token_endpoint(&request.path) {
            let mut response = self.handle_token(request);
            response.latency = verdict.delay;
            return response;
        }

        // 3. Authorization.
        if let Some(error) = self.check_auth(request) {
            let mut response = error_response(&error);
            response.latency = verdict.delay;
            return response;
        }

        // 4. Route, so the quota cost is known…
        let Some((user, route)) = Route::of(request) else {
            let mut response = error_response(&ApiError::NotFound);
            response.latency = verdict.delay;
            return response;
        };
        // …and a request addressed to somebody else's mailbox is a 404, not a
        // silent redirect to this one. Gmail accepts `me` or the authenticated
        // address and nothing else.
        if user != "me" && user != self.config.email_address {
            let mut response = error_response(&ApiError::NotFound);
            response.latency = verdict.delay;
            return response;
        }
        // …and a batch inside a batch is refused before it can recurse.
        if matches!(route, Route::Batch) && in_batch {
            let mut response = error_response(&ApiError::InvalidArgument(
                "Batch requests cannot be nested.".to_owned(),
            ));
            response.latency = verdict.delay;
            return response;
        }

        // 5. Quota.
        if let Some(error) = self.lock().quota.charge(route.cost(), now) {
            let mut response = error_response(&error);
            if error == ApiError::TooManyRequests {
                response = response.with_header("Retry-After", "1");
            }
            response.latency = verdict.delay;
            return response;
        }

        // 6. The work.
        let mut response = match route {
            Route::Profile => self.handle_profile(),
            Route::LabelsList => self.handle_labels_list(),
            Route::LabelGet(id) => self.handle_label_get(&id),
            Route::MessagesList => self.handle_messages_list(request, now),
            Route::MessageGet(id) => self.handle_message_get(request, &id),
            Route::AttachmentGet(message, attachment) => {
                self.handle_attachment_get(&message, &attachment)
            }
            Route::ThreadsList => self.handle_threads_list(request, now),
            Route::ThreadGet(id) => self.handle_thread_get(request, &id),
            Route::HistoryList => self.handle_history_list(request),
            Route::Watch => self.handle_watch(request, now),
            Route::Stop => self.handle_stop(),
            Route::Batch => self.handle_batch(request),
        };
        response.latency = verdict.delay;
        response
    }

    fn apply_fault(&self, fault: &Fault) -> SimResponse {
        match fault {
            Fault::Fail(error) => error_response(error),
            Fault::Throttle {
                retry_after_seconds,
            } => {
                let mut response = error_response(&ApiError::TooManyRequests);
                if let Some(seconds) = retry_after_seconds {
                    response = response.with_header("Retry-After", seconds.to_string());
                }
                response
            }
            Fault::Unavailable {
                retry_after_seconds,
            } => {
                let mut response = error_response(&ApiError::ServiceUnavailable);
                if let Some(seconds) = retry_after_seconds {
                    response = response.with_header("Retry-After", seconds.to_string());
                }
                response
            }
            Fault::ExpireToken => {
                self.lock().auth.access_token_expired = true;
                error_response(&ApiError::InvalidCredentials)
            }
            // A delay is handled by the caller; reaching here means a delay was
            // treated as a response fault, which cannot happen.
            Fault::Delay(_) => SimResponse::json(200, &json!({})),
            Fault::Custom {
                status,
                content_type,
                body,
            } => SimResponse::raw(*status, content_type, body.clone()),
        }
    }

    /// `None` when the request may proceed.
    ///
    /// A missing header, the wrong token and an expired token are all the same
    /// `401 authError` — Gmail does not distinguish them, and a client must not
    /// need it to.
    fn check_auth(&self, request: &SimRequest) -> Option<ApiError> {
        if self.config.auth == AuthMode::Off {
            return None;
        }
        let inner = self.lock();
        match request.bearer_token() {
            Some(token) if token == inner.auth.access_token && !inner.auth.access_token_expired => {
                None
            }
            _ => Some(ApiError::InvalidCredentials),
        }
    }

    // -- endpoints ---------------------------------------------------------

    fn handle_profile(&self) -> SimResponse {
        let inner = self.lock();
        SimResponse::json(
            200,
            &render::profile_json(
                &inner.mailbox.email_address,
                inner.mailbox.message_count(),
                inner.mailbox.thread_count(),
                inner.mailbox.history_id(),
            ),
        )
    }

    fn handle_labels_list(&self) -> SimResponse {
        let inner = self.lock();
        let labels: Vec<Value> = inner
            .mailbox
            .labels()
            .iter()
            .map(crate::label::Label::list_json)
            .collect();
        SimResponse::json(200, &json!({ "labels": labels }))
    }

    fn handle_label_get(&self, id: &str) -> SimResponse {
        let inner = self.lock();
        let Some(label) = inner.mailbox.label(id) else {
            return error_response(&ApiError::NotFound);
        };
        let counts = inner.mailbox.label_counts(id);
        SimResponse::json(200, &label.get_json(counts))
    }

    fn handle_messages_list(&self, request: &SimRequest, now: i64) -> SimResponse {
        let raw_query = request.query_one("q").unwrap_or_default().to_owned();
        let include_spam_trash = request.query_one("includeSpamTrash") == Some("true");
        let label_filter: Vec<String> = request
            .query_all("labelIds")
            .into_iter()
            .map(ToOwned::to_owned)
            .collect();
        let max_results = clamp_max_results(
            request.query_one("maxResults"),
            LIST_DEFAULT_MAX,
            LIST_CEILING,
        );
        let scope = fingerprint(&[
            &raw_query,
            if include_spam_trash { "1" } else { "0" },
            &label_filter.join("\u{1}"),
        ]);

        let cursor = match request.query_one("pageToken") {
            None => None,
            Some(token) => match PageCursor::decode(token) {
                Some(cursor) if cursor.scope == scope => Some(cursor),
                // EDGE: a token from another query, or garbage. Restarting at
                // page 1 would turn a client's paging bug into an infinite loop
                // that looks like success.
                _ => return error_response(&invalid_page_token()),
            },
        };

        let inner = self.lock();
        // EDGE: `labelIds` is **case-sensitive** and an unknown id is a `400`,
        // not an empty result. `labelIds=sent` is the classic version of this —
        // Gmail answers `Invalid label: sent` and means `SENT`. Matching nothing
        // instead would let a client that lowercases its label ids look like a
        // mailbox that simply has no mail in that label.
        if let Some(bad) = unknown_label(&inner.mailbox, &label_filter) {
            return error_response(&ApiError::InvalidArgument(format!("Invalid label: {bad}")));
        }
        let parsed = Query::parse(&raw_query);
        let all = inner
            .mailbox
            .search(&parsed, now, include_spam_trash, &label_filter);
        let after: Vec<&crate::StoredMessage> = all
            .into_iter()
            .filter(|message| after_cursor(message, cursor.as_ref()))
            .collect();

        let page: Vec<&crate::StoredMessage> = after.iter().take(max_results).copied().collect();
        let next = (after.len() > page.len())
            .then(|| page.last())
            .flatten()
            .map(|last| {
                PageCursor {
                    scope,
                    after_internal_date_ms: last.internal_date_ms,
                    after_id: last.id.clone(),
                }
                .encode()
            });

        let entries: Vec<Value> = page
            .iter()
            .map(|message| render::message_reference(message))
            .collect();
        let estimate =
            self.config
                .result_size_estimate
                .of(entries.len(), max_results, next.is_some());
        SimResponse::json(
            200,
            &render::list_envelope("messages", entries, next.as_deref(), estimate),
        )
    }

    fn handle_message_get(&self, request: &SimRequest, id: &str) -> SimResponse {
        let format = Format::parse(request.query_one("format"));
        let metadata_headers: Vec<String> = request
            .query_all("metadataHeaders")
            .into_iter()
            .map(ToOwned::to_owned)
            .collect();
        let inner = self.lock();
        // EDGE: a *trashed* message is still here. Only a permanently deleted
        // one is a 404, which is why `messages.list` hiding it is the only
        // signal a client gets for Trash.
        let Some(message) = inner.mailbox.message(id) else {
            return error_response(&ApiError::NotFound);
        };
        SimResponse::json(
            200,
            &render::message_json(message, format, &metadata_headers),
        )
    }

    fn handle_attachment_get(&self, message_id: &str, attachment_id: &str) -> SimResponse {
        let Some((owner, part_id)) = parse_attachment_id(attachment_id) else {
            return error_response(&ApiError::NotFound);
        };
        // An attachment id from another message must not resolve here; Gmail
        // scopes attachments to their message.
        if owner != message_id {
            return error_response(&ApiError::NotFound);
        }
        let inner = self.lock();
        let Some(message) = inner.mailbox.message(message_id) else {
            return error_response(&ApiError::NotFound);
        };
        let tree = message.parts();
        let Some(part) = tree
            .walk()
            .into_iter()
            .find(|part| part.part_id == part_id && part.is_attachment)
        else {
            return error_response(&ApiError::NotFound);
        };
        SimResponse::json(
            200,
            &json!({
                "size": part.body.len(),
                "data": crate::ids::b64url(&part.body),
            }),
        )
    }

    fn handle_threads_list(&self, request: &SimRequest, now: i64) -> SimResponse {
        let raw_query = request.query_one("q").unwrap_or_default().to_owned();
        let include_spam_trash = request.query_one("includeSpamTrash") == Some("true");
        let label_filter: Vec<String> = request
            .query_all("labelIds")
            .into_iter()
            .map(ToOwned::to_owned)
            .collect();
        let max_results = clamp_max_results(
            request.query_one("maxResults"),
            LIST_DEFAULT_MAX,
            LIST_CEILING,
        );
        let scope = fingerprint(&[
            "threads",
            &raw_query,
            if include_spam_trash { "1" } else { "0" },
            &label_filter.join("\u{1}"),
        ]);
        let cursor = match request.query_one("pageToken") {
            None => None,
            Some(token) => match PageCursor::decode(token) {
                Some(cursor) if cursor.scope == scope => Some(cursor),
                _ => return error_response(&invalid_page_token()),
            },
        };

        let inner = self.lock();
        // Same case-sensitive `labelIds` rule as `messages.list`.
        if let Some(bad) = unknown_label(&inner.mailbox, &label_filter) {
            return error_response(&ApiError::InvalidArgument(format!("Invalid label: {bad}")));
        }
        let parsed = Query::parse(&raw_query);
        let matched = inner
            .mailbox
            .search(&parsed, now, include_spam_trash, &label_filter);

        // A thread is in the result when any of its messages is, and it sorts by
        // its newest matching message.
        let mut threads: Vec<(i64, String, String)> = Vec::new();
        for message in matched {
            if threads.iter().any(|(_, id, _)| *id == message.thread_id) {
                continue;
            }
            threads.push((
                message.internal_date_ms,
                message.thread_id.clone(),
                message.id.clone(),
            ));
        }
        let after: Vec<&(i64, String, String)> = threads
            .iter()
            .filter(|(date, _, newest)| match cursor.as_ref() {
                None => true,
                Some(cursor) => {
                    *date < cursor.after_internal_date_ms
                        || (*date == cursor.after_internal_date_ms && *newest < cursor.after_id)
                }
            })
            .collect();
        let page: Vec<&(i64, String, String)> = after.iter().take(max_results).copied().collect();
        let next = (after.len() > page.len())
            .then(|| page.last())
            .flatten()
            .map(|(date, _, newest)| {
                PageCursor {
                    scope,
                    after_internal_date_ms: *date,
                    after_id: newest.clone(),
                }
                .encode()
            });

        let entries: Vec<Value> = page
            .iter()
            .map(|(_, thread_id, _)| {
                let messages = inner.mailbox.thread(thread_id);
                render::thread_reference(thread_id, &messages)
            })
            .collect();
        let estimate =
            self.config
                .result_size_estimate
                .of(entries.len(), max_results, next.is_some());
        SimResponse::json(
            200,
            &render::list_envelope("threads", entries, next.as_deref(), estimate),
        )
    }

    fn handle_thread_get(&self, request: &SimRequest, id: &str) -> SimResponse {
        let format = Format::parse(request.query_one("format"));
        let metadata_headers: Vec<String> = request
            .query_all("metadataHeaders")
            .into_iter()
            .map(ToOwned::to_owned)
            .collect();
        let inner = self.lock();
        let messages = inner.mailbox.thread(id);
        if messages.is_empty() {
            return error_response(&ApiError::NotFound);
        }
        SimResponse::json(
            200,
            &render::thread_json(id, &messages, format, &metadata_headers),
        )
    }

    fn handle_history_list(&self, request: &SimRequest) -> SimResponse {
        let Some(raw_start) = request.query_one("startHistoryId") else {
            return error_response(&ApiError::InvalidArgument(
                "Required parameter: startHistoryId".to_owned(),
            ));
        };
        let Ok(start) = raw_start.trim().parse::<u64>() else {
            return error_response(&ApiError::InvalidArgument(format!(
                "Invalid startHistoryId: {raw_start}"
            )));
        };
        let types: Vec<HistoryType> = {
            let asked: Vec<HistoryType> = request
                .query_all("historyTypes")
                .into_iter()
                .filter_map(HistoryType::parse)
                .collect();
            if asked.is_empty() {
                HistoryType::ALL.to_vec()
            } else {
                asked
            }
        };
        let label_filter = request.query_one("labelId").map(ToOwned::to_owned);
        let max_results = clamp_max_results(
            request.query_one("maxResults"),
            HISTORY_DEFAULT_MAX,
            HISTORY_CEILING,
        );
        let scope = fingerprint(&[
            raw_start,
            &types
                .iter()
                .map(|kind| kind.as_str())
                .collect::<Vec<_>>()
                .join(","),
            label_filter.as_deref().unwrap_or(""),
        ]);
        let cursor = match request.query_one("pageToken") {
            None => None,
            Some(token) => match HistoryCursor::decode(token) {
                Some(cursor) if cursor.scope == scope => Some(cursor),
                _ => return error_response(&invalid_page_token()),
            },
        };

        let inner = self.lock();
        let log = inner.mailbox.history();

        // The whole point of the 404 path: the cursor is older than the window
        // Gmail still holds, so nothing short of a full re-sync is correct.
        if !log.can_serve(start) {
            return error_response(&ApiError::NotFound);
        }

        let from = cursor.map_or(start, |cursor| cursor.after_history_id.max(start));
        let matching: Vec<HistoryRecord> = log
            .since(from)
            .into_iter()
            .filter_map(|record| record.filtered(&types))
            .filter_map(|record| filter_by_label(record, label_filter.as_deref()))
            .collect();

        let page: Vec<&HistoryRecord> = matching.iter().take(max_results).collect();
        let next = (matching.len() > page.len())
            .then(|| page.last())
            .flatten()
            .map(|last| {
                HistoryCursor {
                    scope,
                    after_history_id: last.id,
                }
                .encode()
            });

        let mut object = serde_json::Map::new();
        if !page.is_empty() {
            object.insert(
                "history".to_owned(),
                Value::Array(page.iter().map(|record| record.json()).collect()),
            );
        }
        if let Some(token) = next {
            object.insert("nextPageToken".to_owned(), json!(token));
        }
        // EDGE: this is the mailbox's *current* historyId on **every** page, not
        // the last record on this one. A client that stores it after page 1 and
        // stops skips every record on page 2. Gmail does exactly this, and the
        // only safe cursor is the last record's own id.
        object.insert(
            "historyId".to_owned(),
            json!(inner.mailbox.history_id().to_string()),
        );
        SimResponse::json(200, &Value::Object(object))
    }

    fn handle_watch(&self, request: &SimRequest, now: i64) -> SimResponse {
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let topic = body
            .get("topicName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if topic.is_empty() {
            return error_response(&ApiError::InvalidArgument(
                "Invalid topicName does not match projects/*/topics/*".to_owned(),
            ));
        }
        let label_ids: Vec<String> = body
            .get("labelIds")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default();

        let expiration_ms = now.saturating_add(self.config.watch_ttl_ms);
        let mut inner = self.lock();
        inner.watch = Some(Watch {
            topic_name: topic,
            label_ids,
            expiration_ms,
        });
        SimResponse::json(
            200,
            &json!({
                // Both strings. `expiration` is epoch **milliseconds**, which is
                // the units people most often get wrong.
                "historyId": inner.mailbox.history_id().to_string(),
                "expiration": expiration_ms.to_string(),
            }),
        )
    }

    fn handle_stop(&self) -> SimResponse {
        // EDGE: stopping a mailbox that was never watched is a 204, not a 404.
        self.lock().watch = None;
        SimResponse::no_content()
    }

    fn handle_batch(&self, request: &SimRequest) -> SimResponse {
        let content_type = request.header_value("content-type").unwrap_or_default();
        let subs = match batch::parse_request(content_type, &request.body) {
            Ok(subs) => subs,
            Err(error) => {
                return error_response(&ApiError::InvalidArgument(error.message()));
            }
        };

        let mut parts = Vec::with_capacity(subs.len());
        let mut total_latency = Duration::ZERO;
        for sub in subs {
            // A sub-request inherits the batch's authorization unless it brought
            // its own, which is what Google does.
            let mut inner_request = sub.request;
            if inner_request.bearer_token().is_none() {
                if let Some(authorization) = request.header_value("authorization") {
                    inner_request = inner_request.header("authorization", authorization);
                }
            }
            let response = self.dispatch(&inner_request, true);
            total_latency += response.latency;
            self.lock().log.push(CallRecord {
                method: inner_request.method,
                target: inner_request.target(),
                status: response.status,
                in_batch: true,
            });
            parts.push((sub.content_id, response));
        }

        let seq = {
            let mut inner = self.lock();
            inner.batch_seq += 1;
            inner.batch_seq
        };
        let (content_type, body) = batch::build_response(seq, self.config.batch_order, parts);
        // The batch itself is a 200 even when every part inside it failed.
        let mut response = SimResponse::raw(200, &content_type, body);
        response.latency = total_latency;
        response
    }

    fn handle_token(&self, request: &SimRequest) -> SimResponse {
        if request.method != Method::Post {
            return SimResponse::json(
                405,
                &json!({ "error": "invalid_request", "error_description": "POST required" }),
            );
        }
        let form = parse_query(&String::from_utf8_lossy(&request.body));
        let field = |name: &str| {
            form.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
                .unwrap_or_default()
        };

        if field("grant_type") != "refresh_token" {
            return SimResponse::json(
                400,
                &json!({
                    "error": "unsupported_grant_type",
                    "error_description": "Invalid grant_type",
                }),
            );
        }

        let mut inner = self.lock();
        if !inner.auth.refresh_valid || field("refresh_token") != inner.auth.refresh_token {
            // The `needs_reauth` trigger. Nothing about the mailbox changes; the
            // client must send the user back through consent.
            return SimResponse::json(
                400,
                &json!({
                    "error": "invalid_grant",
                    "error_description": "Token has been expired or revoked.",
                }),
            );
        }

        inner.auth.issued += 1;
        let issued = inner.auth.issued;
        inner.auth.access_token = format!("ya29.sim-access-{issued}");
        inner.auth.access_token_expired = false;
        let mut body = json!({
            "access_token": inner.auth.access_token,
            "expires_in": 3599,
            "scope": "https://www.googleapis.com/auth/gmail.readonly",
            "token_type": "Bearer",
        });
        if inner.auth.rotate_refresh_token {
            // Rotation: a client that does not persist the new refresh token
            // works exactly once and then needs re-consent.
            inner.auth.refresh_token = format!("1//sim-refresh-{issued}");
            body["refresh_token"] = json!(inner.auth.refresh_token);
        }
        SimResponse::json(200, &body)
    }
}

impl GmailApi for Simulator {
    fn call(&self, request: SimRequest) -> impl std::future::Future<Output = SimResponse> + Send {
        // The simulator has no I/O, so this is ready immediately. `Delay` faults
        // are reported in `SimResponse::latency`; only the HTTP transport turns
        // them into real waiting.
        std::future::ready(self.handle(&request))
    }
}

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Route {
    Profile,
    LabelsList,
    LabelGet(String),
    MessagesList,
    MessageGet(String),
    AttachmentGet(String, String),
    ThreadsList,
    ThreadGet(String),
    HistoryList,
    Watch,
    Stop,
    Batch,
}

impl Route {
    fn cost(&self) -> u32 {
        match self {
            Self::Profile => cost::GET_PROFILE,
            Self::LabelsList => cost::LABELS_LIST,
            Self::LabelGet(_) => cost::LABELS_GET,
            Self::MessagesList => cost::MESSAGES_LIST,
            Self::MessageGet(_) => cost::MESSAGES_GET,
            Self::AttachmentGet(..) => cost::ATTACHMENTS_GET,
            Self::ThreadsList => cost::THREADS_LIST,
            Self::ThreadGet(_) => cost::THREADS_GET,
            Self::HistoryList => cost::HISTORY_LIST,
            Self::Watch => cost::WATCH,
            Self::Stop => cost::STOP,
            // The batch envelope itself is free; every sub-request pays.
            Self::Batch => 0,
        }
    }

    /// Route a request, returning the `userId` segment alongside it so the
    /// caller can check the mailbox is the one being addressed.
    fn of(request: &SimRequest) -> Option<(String, Self)> {
        let segments: Vec<&str> = request
            .path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect();

        if request.method == Method::Post && matches!(segments.as_slice(), ["batch", "gmail", "v1"])
        {
            return Some(("me".to_owned(), Self::Batch));
        }

        let ["gmail", "v1", "users", rest @ ..] = segments.as_slice() else {
            return None;
        };
        let (user, rest) = rest.split_first()?;
        let route = Self::endpoint(request.method, rest)?;
        Some(((*user).to_owned(), route))
    }

    fn endpoint(method: Method, rest: &[&str]) -> Option<Self> {
        match (method, rest) {
            (Method::Get, ["profile"]) => Some(Self::Profile),
            (Method::Get, ["labels"]) => Some(Self::LabelsList),
            (Method::Get, ["labels", id]) => Some(Self::LabelGet((*id).to_owned())),
            (Method::Get, ["messages"]) => Some(Self::MessagesList),
            (Method::Get, ["messages", id]) => Some(Self::MessageGet((*id).to_owned())),
            (Method::Get, ["messages", id, "attachments", attachment]) => Some(
                Self::AttachmentGet((*id).to_owned(), (*attachment).to_owned()),
            ),
            (Method::Get, ["threads"]) => Some(Self::ThreadsList),
            (Method::Get, ["threads", id]) => Some(Self::ThreadGet((*id).to_owned())),
            (Method::Get, ["history"]) => Some(Self::HistoryList),
            (Method::Post, ["watch"]) => Some(Self::Watch),
            (Method::Post, ["stop"]) => Some(Self::Stop),
            _ => None,
        }
    }
}

fn is_token_endpoint(path: &str) -> bool {
    matches!(path, "/token" | "/oauth2/v4/token" | "/o/oauth2/token")
}

fn error_response(error: &ApiError) -> SimResponse {
    SimResponse::json(error.status(), &error.body())
}

fn invalid_page_token() -> ApiError {
    ApiError::InvalidArgument("Invalid pageToken".to_owned())
}

/// `maxResults`: Gmail's default when absent or unparseable, its ceiling when
/// too large, and `0` also means the default.
fn clamp_max_results(raw: Option<&str>, default: usize, ceiling: usize) -> usize {
    raw.and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
        .min(ceiling)
}

/// The first `labelIds` value the mailbox does not have, matched **exactly**.
///
/// Case-sensitive on purpose: `labelIds` is, even though `q=label:` is not.
fn unknown_label(mailbox: &Mailbox, wanted: &[String]) -> Option<String> {
    wanted
        .iter()
        .find(|id| mailbox.label(id).is_none())
        .cloned()
}

/// Is this message strictly after the cursor in `(internalDate desc, id desc)`?
fn after_cursor(message: &crate::StoredMessage, cursor: Option<&PageCursor>) -> bool {
    match cursor {
        None => true,
        Some(cursor) => {
            message.internal_date_ms < cursor.after_internal_date_ms
                || (message.internal_date_ms == cursor.after_internal_date_ms
                    && message.id < cursor.after_id)
        }
    }
}

/// `labelId` on `history.list`: keep only the changes about messages carrying
/// that label at the time of the change.
fn filter_by_label(record: HistoryRecord, label: Option<&str>) -> Option<HistoryRecord> {
    let Some(label) = label else {
        return Some(record);
    };
    let keep = |ids: &[String]| ids.iter().any(|id| id == label);
    let filtered = HistoryRecord {
        id: record.id,
        messages_added: record
            .messages_added
            .into_iter()
            .filter(|stub| keep(&stub.label_ids))
            .collect(),
        messages_deleted: record
            .messages_deleted
            .into_iter()
            .filter(|stub| keep(&stub.label_ids))
            .collect(),
        labels_added: record
            .labels_added
            .into_iter()
            .filter(|change| keep(&change.message.label_ids))
            .collect(),
        labels_removed: record
            .labels_removed
            .into_iter()
            .filter(|change| keep(&change.message.label_ids))
            .collect(),
    };
    HistoryType::ALL
        .into_iter()
        .any(|kind| filtered.has(kind))
        .then_some(filtered)
}

/// Which labels are user-created, for `has:userlabels`.
#[must_use]
pub fn user_labels(mailbox: &Mailbox) -> Vec<String> {
    mailbox
        .labels()
        .iter()
        .filter(|label| label.kind == LabelType::User)
        .map(|label| label.id.clone())
        .collect()
}
