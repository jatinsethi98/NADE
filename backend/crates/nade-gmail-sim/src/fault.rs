//! Failure on purpose, deterministically.
//!
//! Every rule fires on a **counted, matched** call, never on a probability.
//! Running the same script twice therefore produces the same failures in the
//! same places, which is what makes a retry test mean anything: a flaky
//! simulator turns a client bug into "try it again".
//!
//! Two shapes of rule:
//!
//! * a **response fault** replaces the work with an error;
//! * a **delay** lets the work happen but reports latency, so a client's timeout
//!   handling can be exercised over the HTTP transport.

use std::time::Duration;

use crate::{api::Method, error::ApiError};

/// What a matched rule does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// Reply with one of Gmail's errors instead of doing the work.
    Fail(ApiError),
    /// `429`, optionally with `Retry-After` in seconds.
    ///
    /// Gmail sends `Retry-After` on some throttles and not others, so the
    /// `None` case is real and a client must cope with it.
    Throttle {
        /// Seconds for the `Retry-After` header, if any.
        retry_after_seconds: Option<u32>,
    },
    /// `503`, optionally with `Retry-After`.
    Unavailable {
        /// Seconds for the `Retry-After` header, if any.
        retry_after_seconds: Option<u32>,
    },
    /// `401`, **and** the access token is marked expired so every later call
    /// fails too until the client refreshes. A one-shot 401 that heals itself
    /// would let a client with no refresh path pass.
    ExpireToken,
    /// Do the work, but report this much latency.
    Delay(Duration),
    /// An arbitrary status and body — a truncated JSON document, an HTML error
    /// page from a proxy, whatever a test needs.
    Custom {
        /// HTTP status.
        status: u16,
        /// `Content-Type` for the reply.
        content_type: String,
        /// The bytes.
        body: Vec<u8>,
    },
}

/// One condition a request must meet for a rule to be eligible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    /// The path contains this substring.
    PathContains(String),
    /// The path is exactly this.
    PathEquals(String),
    /// The HTTP method matches.
    IsMethod(Method),
    /// A query parameter is present with this exact value.
    QueryEquals(String, String),
    /// A query parameter is present at all.
    QueryPresent(String),
    /// The request is a batch **sub**-request, or is not.
    InBatch(bool),
}

impl Condition {
    fn holds(&self, request: &crate::api::SimRequest, in_batch: bool) -> bool {
        match self {
            Self::PathContains(needle) => request.path.contains(needle.as_str()),
            Self::PathEquals(path) => request.path == *path,
            Self::IsMethod(method) => request.method == *method,
            Self::QueryEquals(name, value) => {
                request.query_one(name).is_some_and(|found| found == value)
            }
            Self::QueryPresent(name) => request.query_one(name).is_some(),
            Self::InBatch(wanted) => in_batch == *wanted,
        }
    }
}

/// A rule: what must be true, how many eligible calls to let past first, and how
/// many times to fire.
#[derive(Debug, Clone)]
pub struct FaultRule {
    conditions: Vec<Condition>,
    skip: u64,
    limit: Option<u32>,
    fault: Fault,
    /// Eligible calls seen so far. Part of the rule's identity, so a rule is
    /// stateful and a simulator's fault table is not shareable between runs.
    seen: u64,
    fired: u32,
}

impl FaultRule {
    /// A rule that fires on every call, forever.
    #[must_use]
    pub fn new(fault: Fault) -> Self {
        Self {
            conditions: Vec::new(),
            skip: 0,
            limit: None,
            fault,
            seen: 0,
            fired: 0,
        }
    }

    /// Restrict to requests whose path contains `needle`.
    #[must_use]
    pub fn on_path_containing(mut self, needle: impl Into<String>) -> Self {
        self.conditions.push(Condition::PathContains(needle.into()));
        self
    }

    /// Restrict to an exact path.
    #[must_use]
    pub fn on_path(mut self, path: impl Into<String>) -> Self {
        self.conditions.push(Condition::PathEquals(path.into()));
        self
    }

    /// Restrict to a method.
    #[must_use]
    pub fn on_method(mut self, method: Method) -> Self {
        self.conditions.push(Condition::IsMethod(method));
        self
    }

    /// Restrict to a query parameter value, e.g. `("format", "raw")`.
    #[must_use]
    pub fn on_query(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.conditions
            .push(Condition::QueryEquals(name.into(), value.into()));
        self
    }

    /// Restrict to batch sub-requests, or to top-level requests.
    #[must_use]
    pub fn in_batch(mut self, inside: bool) -> Self {
        self.conditions.push(Condition::InBatch(inside));
        self
    }

    /// Add any condition.
    #[must_use]
    pub fn when(mut self, condition: Condition) -> Self {
        self.conditions.push(condition);
        self
    }

    /// Let this many eligible calls through before firing.
    #[must_use]
    pub fn after(mut self, calls: u64) -> Self {
        self.skip = calls;
        self
    }

    /// Fire at most this many times.
    #[must_use]
    pub fn times(mut self, count: u32) -> Self {
        self.limit = Some(count);
        self
    }

    /// Fire on exactly the `n`th matching call (1-based) and no other.
    #[must_use]
    pub fn on_nth_call(self, n: u64) -> Self {
        self.after(n.saturating_sub(1)).times(1)
    }

    /// Has this rule finished?
    #[must_use]
    pub fn is_spent(&self) -> bool {
        self.limit.is_some_and(|limit| self.fired >= limit)
    }

    /// Test the rule against a request, counting the call if it is eligible.
    fn evaluate(&mut self, request: &crate::api::SimRequest, in_batch: bool) -> Option<&Fault> {
        if self.is_spent() {
            return None;
        }
        if !self
            .conditions
            .iter()
            .all(|condition| condition.holds(request, in_batch))
        {
            return None;
        }
        self.seen += 1;
        if self.seen <= self.skip {
            return None;
        }
        self.fired += 1;
        Some(&self.fault)
    }
}

/// The whole fault table.
#[derive(Debug, Clone, Default)]
pub struct FaultTable {
    rules: Vec<FaultRule>,
}

/// What the table decided for one request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Verdict {
    /// The response fault to apply, if any. The **first** matching rule wins.
    pub fault: Option<Fault>,
    /// Total delay from every matching delay rule.
    pub delay: Duration,
}

impl FaultTable {
    /// Add a rule. Rules are consulted in the order they were added.
    pub fn push(&mut self, rule: FaultRule) {
        self.rules.push(rule);
    }

    /// Drop every rule, spent or not.
    pub fn clear(&mut self) {
        self.rules.clear();
    }

    /// How many rules are still live.
    #[must_use]
    pub fn live(&self) -> usize {
        self.rules.iter().filter(|rule| !rule.is_spent()).count()
    }

    /// Decide what happens to this request.
    ///
    /// Delays accumulate across every matching rule; the response fault is the
    /// first one that matches. Later rules still get their counters advanced,
    /// because "the third `messages.get` fails" must mean the third call the
    /// client made, not the third one that got past an earlier rule.
    pub fn verdict(&mut self, request: &crate::api::SimRequest, in_batch: bool) -> Verdict {
        let mut verdict = Verdict::default();
        for rule in &mut self.rules {
            let Some(fault) = rule.evaluate(request, in_batch) else {
                continue;
            };
            match fault {
                Fault::Delay(by) => verdict.delay += *by,
                other => {
                    if verdict.fault.is_none() {
                        verdict.fault = Some(other.clone());
                    }
                }
            }
        }
        verdict
    }
}

// ---------------------------------------------------------------------------
// Quota
// ---------------------------------------------------------------------------

/// Gmail's documented per-user ceiling.
pub const UNITS_PER_SECOND: f64 = 250.0;

/// Published quota unit costs, per method.
///
/// The ones NADE actually calls are the ones that matter: `messages.get` and
/// `messages.list` cost 5 each, so 250 units a second is fifty gets a second,
/// not fifty *requests*. A client that rate-limits by requests will pass a naive
/// stub and be throttled by Gmail.
pub mod cost {
    /// `users.getProfile`.
    pub const GET_PROFILE: u32 = 1;
    /// `users.labels.list`.
    pub const LABELS_LIST: u32 = 1;
    /// `users.labels.get`.
    pub const LABELS_GET: u32 = 1;
    /// `users.history.list`.
    pub const HISTORY_LIST: u32 = 2;
    /// `users.messages.list`.
    pub const MESSAGES_LIST: u32 = 5;
    /// `users.messages.get`.
    pub const MESSAGES_GET: u32 = 5;
    /// `users.messages.attachments.get`.
    pub const ATTACHMENTS_GET: u32 = 5;
    /// `users.threads.list`.
    pub const THREADS_LIST: u32 = 10;
    /// `users.threads.get`.
    pub const THREADS_GET: u32 = 10;
    /// `users.watch`.
    pub const WATCH: u32 = 100;
    /// `users.stop`.
    pub const STOP: u32 = 50;
}

/// Which error an over-quota call gets.
///
/// Gmail is not consistent here — it has used `403 rateLimitExceeded`,
/// `403 userRateLimitExceeded` and `429` for the same condition at different
/// times — so the simulator lets a test pick, and defaults to the `429` a modern
/// client is most likely to meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverQuota {
    /// `429 rateLimitExceeded` with a `Retry-After`.
    #[default]
    TooManyRequests,
    /// `403 userRateLimitExceeded`.
    UserRateLimit,
    /// `403 rateLimitExceeded`.
    ProjectRateLimit,
}

impl OverQuota {
    /// The error this maps to.
    #[must_use]
    pub fn error(self) -> ApiError {
        match self {
            Self::TooManyRequests => ApiError::TooManyRequests,
            Self::UserRateLimit => ApiError::UserRateLimitExceeded,
            Self::ProjectRateLimit => ApiError::RateLimitExceeded,
        }
    }
}

/// A token bucket over quota units, driven entirely by the simulator's clock.
#[derive(Debug, Clone)]
pub struct Quota {
    /// Whether the bucket is consulted at all.
    pub enabled: bool,
    /// Refill rate, and the bucket's capacity.
    pub units_per_second: f64,
    /// What an over-quota call gets back.
    pub over_quota: OverQuota,
    units: f64,
    last_ms: i64,
}

impl Quota {
    /// A bucket at Gmail's real rate, starting full at `now_ms`.
    ///
    /// Disabled by default: most tests are not about quota, and a bucket that
    /// silently throttled them would make every failure ambiguous. Turn it on
    /// with [`Self::enabled`].
    #[must_use]
    pub fn new(now_ms: i64) -> Self {
        Self {
            enabled: false,
            units_per_second: UNITS_PER_SECOND,
            over_quota: OverQuota::default(),
            units: UNITS_PER_SECOND,
            last_ms: now_ms,
        }
    }

    /// Units currently available.
    #[must_use]
    pub fn available(&self) -> f64 {
        self.units
    }

    /// Refill for elapsed time, then try to debit `cost`.
    ///
    /// Returns `None` when the call may proceed and the over-quota error when it
    /// may not. Nothing is debited on rejection, so the bucket state stays a
    /// simple function of the clock and the calls that succeeded.
    pub fn charge(&mut self, cost: u32, now_ms: i64) -> Option<ApiError> {
        if !self.enabled {
            return None;
        }
        // EDGE: a clock that went backwards contributes no refill rather than a
        // negative one. A test is allowed to set time backwards, and a negative
        // refill would leave the bucket permanently poisoned.
        let elapsed_ms = (now_ms - self.last_ms).max(0);
        self.last_ms = now_ms;
        #[allow(clippy::cast_precision_loss)]
        let refill = (elapsed_ms as f64) / 1000.0 * self.units_per_second;
        self.units = (self.units + refill).min(self.units_per_second);

        if self.units < f64::from(cost) {
            return Some(self.over_quota.error());
        }
        self.units -= f64::from(cost);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::SimRequest;

    fn get(path: &str) -> SimRequest {
        SimRequest::get(path)
    }

    #[test]
    fn a_rule_with_no_conditions_fires_on_everything() {
        let mut table = FaultTable::default();
        table.push(FaultRule::new(Fault::Fail(ApiError::BackendError)));
        assert!(table.verdict(&get("/a"), false).fault.is_some());
        assert!(table.verdict(&get("/b"), false).fault.is_some());
    }

    #[test]
    fn nth_call_fires_once_at_exactly_the_right_place() {
        let mut table = FaultTable::default();
        table.push(
            FaultRule::new(Fault::Throttle {
                retry_after_seconds: Some(3),
            })
            .on_nth_call(3),
        );
        let outcomes: Vec<bool> = (0..5)
            .map(|_| table.verdict(&get("/x"), false).fault.is_some())
            .collect();
        assert_eq!(outcomes, [false, false, true, false, false]);
        assert_eq!(table.live(), 0, "a spent rule is done");
    }

    #[test]
    fn path_conditions_count_only_matching_calls() {
        let mut table = FaultTable::default();
        table.push(
            FaultRule::new(Fault::Fail(ApiError::NotFound))
                .on_path_containing("/messages")
                .on_nth_call(2),
        );
        assert!(table
            .verdict(&get("/gmail/v1/users/me/labels"), false)
            .fault
            .is_none());
        assert!(table
            .verdict(&get("/gmail/v1/users/me/messages"), false)
            .fault
            .is_none());
        assert!(table
            .verdict(&get("/gmail/v1/users/me/labels"), false)
            .fault
            .is_none());
        assert!(
            table
                .verdict(&get("/gmail/v1/users/me/messages/abc"), false)
                .fault
                .is_some(),
            "the second *matching* call, not the second call"
        );
    }

    #[test]
    fn query_conditions_select_a_format() {
        let mut table = FaultTable::default();
        table.push(FaultRule::new(Fault::Fail(ApiError::NotFound)).on_query("format", "raw"));
        assert!(table.verdict(&get("/m?format=full"), false).fault.is_none());
        assert!(table.verdict(&get("/m?format=raw"), false).fault.is_some());
    }

    #[test]
    fn delays_accumulate_and_do_not_block_a_response() {
        let mut table = FaultTable::default();
        table.push(FaultRule::new(Fault::Delay(Duration::from_millis(100))));
        table.push(FaultRule::new(Fault::Delay(Duration::from_millis(50))));
        let verdict = table.verdict(&get("/x"), false);
        assert_eq!(verdict.delay, Duration::from_millis(150));
        assert!(verdict.fault.is_none(), "a delay is not a failure");
    }

    #[test]
    fn the_first_matching_fault_wins_but_later_rules_still_count() {
        let mut table = FaultTable::default();
        table.push(FaultRule::new(Fault::Fail(ApiError::BackendError)).times(1));
        table.push(FaultRule::new(Fault::Fail(ApiError::NotFound)).on_nth_call(1));
        let first = table.verdict(&get("/x"), false);
        assert_eq!(first.fault, Some(Fault::Fail(ApiError::BackendError)));
        // The second rule's counter advanced during that same call, so it is
        // already spent — the client made one call, and both rules saw it.
        let second = table.verdict(&get("/x"), false);
        assert_eq!(second.fault, None);
    }

    #[test]
    fn batch_sub_requests_can_be_targeted_on_their_own() {
        let mut table = FaultTable::default();
        table.push(FaultRule::new(Fault::Fail(ApiError::NotFound)).in_batch(true));
        assert!(table.verdict(&get("/x"), false).fault.is_none());
        assert!(table.verdict(&get("/x"), true).fault.is_some());
    }

    #[test]
    fn clearing_removes_everything() {
        let mut table = FaultTable::default();
        table.push(FaultRule::new(Fault::Fail(ApiError::BackendError)));
        table.clear();
        assert_eq!(table.live(), 0);
        assert!(table.verdict(&get("/x"), false).fault.is_none());
    }

    #[test]
    fn the_quota_ceiling_is_fifty_gets_per_second() {
        let mut quota = Quota::new(0);
        quota.enabled = true;
        for call in 0..50 {
            assert!(
                quota.charge(cost::MESSAGES_GET, 0).is_none(),
                "get {call} must fit in one second"
            );
        }
        assert_eq!(
            quota.charge(cost::MESSAGES_GET, 0),
            Some(ApiError::TooManyRequests),
            "the 51st get in the same second is over quota"
        );
    }

    #[test]
    fn cheap_calls_get_two_hundred_and_fifty_a_second() {
        let mut quota = Quota::new(0);
        quota.enabled = true;
        for _ in 0..250 {
            assert!(quota.charge(cost::GET_PROFILE, 0).is_none());
        }
        assert!(quota.charge(cost::GET_PROFILE, 0).is_some());
    }

    #[test]
    fn the_bucket_refills_with_the_clock_and_not_otherwise() {
        let mut quota = Quota::new(0);
        quota.enabled = true;
        for _ in 0..50 {
            quota.charge(cost::MESSAGES_GET, 0);
        }
        assert!(quota.charge(cost::MESSAGES_GET, 0).is_some());
        // 20 ms buys exactly 5 units: one more get.
        assert!(quota.charge(cost::MESSAGES_GET, 20).is_none());
        assert!(quota.charge(cost::MESSAGES_GET, 20).is_some());
        // A full second refills the bucket completely.
        assert!(quota.charge(cost::MESSAGES_GET, 1_020).is_none());
    }

    #[test]
    fn a_disabled_bucket_never_throttles() {
        let mut quota = Quota::new(0);
        for _ in 0..10_000 {
            assert!(quota.charge(cost::MESSAGES_GET, 0).is_none());
        }
    }

    #[test]
    fn a_backwards_clock_does_not_poison_the_bucket() {
        let mut quota = Quota::new(10_000);
        quota.enabled = true;
        quota.charge(cost::MESSAGES_GET, 10_000);
        let before = quota.available();
        // Time goes backwards; no refill, no *negative* refill either.
        quota.charge(cost::MESSAGES_GET, 0);
        assert!(quota.available() <= before);
        assert!(quota.available() >= 0.0);
    }

    #[test]
    fn the_over_quota_error_is_selectable() {
        for (mode, expected) in [
            (OverQuota::TooManyRequests, ApiError::TooManyRequests),
            (OverQuota::UserRateLimit, ApiError::UserRateLimitExceeded),
            (OverQuota::ProjectRateLimit, ApiError::RateLimitExceeded),
        ] {
            let mut quota = Quota::new(0);
            quota.enabled = true;
            quota.over_quota = mode;
            for _ in 0..50 {
                quota.charge(cost::MESSAGES_GET, 0);
            }
            assert_eq!(quota.charge(cost::MESSAGES_GET, 0), Some(expected));
        }
    }
}
