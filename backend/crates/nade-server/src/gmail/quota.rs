//! Gmail's quota, respected rather than discovered.
//!
//! Gmail rations a user along **two independent axes**, and a limiter that
//! models one of them is not a limiter:
//!
//! 1. **Rate** - 250 quota units per user per second. `messages.get` costs 5,
//!    so the real ceiling is 50 gets per second, not "50 requests", which is
//!    what a naive request-per-second limiter would enforce and which would get
//!    us throttled the moment a batch of 45 lands. [`Bucket`] debits the
//!    **true** cost of each call.
//! 2. **Concurrency** - how many requests for that user are in flight at once.
//!    This is a *count*, not a rate, and no amount of unit accounting can
//!    express it.
//!
//! The first live sync was throttled on axis 2 while comfortably inside axis 1:
//! Gmail answered 91 sub-requests with
//! `429 rateLimitExceeded: "Too many concurrent requests for user."` A
//! `multipart/mixed` batch is one HTTP request to us and **`n` simultaneous
//! operations to Google**, so a 45-wide batch is 45 concurrent requests for one
//! user. The unit budget said 225 of 250 and was right; the concurrency budget
//! was never being kept. [`MAX_CONCURRENT_REQUESTS`] and
//! [`MAX_CONCURRENT_SUBREQUESTS`] are that second budget.
//!
//! Everything here is driven by [`Instant`], which is monotonic, so a
//! wall-clock jump cannot make the limiter either stall or open the floodgates
//! (edge case 8).

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

/// Gmail's per-user, per-second quota.
pub const UNITS_PER_SECOND: f64 = 250.0;

/// How many Gmail HTTP requests this process keeps in flight for one account.
///
/// One. Not a throughput choice - a correctness one: two `gmail_sync` jobs, or
/// a sync and an on-demand fetch, would otherwise overlap and Gmail counts them
/// against the same per-user concurrency budget that produced the live 429s.
/// Every call in this crate is sequential anyway, so this costs nothing and
/// makes "we never ran two Gmail calls at once" true by construction rather
/// than by the shape of the current call sites.
pub const MAX_CONCURRENT_REQUESTS: usize = 1;

/// How many sub-requests one `multipart/mixed` batch may carry.
///
/// Gmail's own protocol limit is 100 and its quota limit is 50 `messages.get`
/// per second, but neither is the binding constraint: **Google expands the
/// batch and runs the sub-requests concurrently**, so the batch width *is* a
/// concurrency figure. 45 produced
/// `"Too many concurrent requests for user."` on the real account. 10 is the
/// value Google's own Gmail guidance uses in its batching examples, and it
/// still costs only 50 units - a fifth of the rate ceiling - so the two axes
/// now have headroom on both.
pub const MAX_CONCURRENT_SUBREQUESTS: usize = 10;

/// Quota units per method, from Google's published table.
pub mod cost {
    pub const GET_PROFILE: u32 = 1;
    pub const LABELS_LIST: u32 = 1;
    pub const HISTORY_LIST: u32 = 2;
    pub const MESSAGES_LIST: u32 = 5;
    pub const MESSAGES_GET: u32 = 5;
    pub const THREADS_GET: u32 = 10;
    pub const ATTACHMENTS_GET: u32 = 5;
}

/// The longest we ever back off on our own initiative.
pub const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// The longest we honour a server-sent `Retry-After`. Beyond this the job
/// should fail and let the queue's own backoff take over rather than hold a
/// worker hostage.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(300);

#[derive(Debug)]
struct State {
    /// May go negative: a debit that overdraws simply pushes the next caller
    /// further into the future. That is what stops a cost larger than the
    /// capacity from deadlocking (an `if units >= cost` loop would spin
    /// forever).
    units: f64,
    last: Instant,
}

/// A token bucket over quota units, plus the concurrency gate.
///
/// Shared through `GmailRuntime`, so every client for every account debits the
/// same budget and queues behind the same gate.
#[derive(Debug)]
pub struct Bucket {
    state: Mutex<State>,
    capacity: f64,
    per_second: f64,
    /// Axis 2. A `Semaphore` rather than a counter because the only useful
    /// answer to "the budget is spent" is *wait*, and waiting has to be async.
    in_flight: tokio::sync::Semaphore,
}

/// Proof that the holder owns one of Gmail's per-user concurrency slots.
///
/// Held for exactly as long as a request is on the wire and dropped before any
/// backoff sleep, so a retrying caller does not sit on the slot while idle.
#[derive(Debug)]
pub struct Slot<'a>(#[allow(dead_code)] tokio::sync::SemaphorePermit<'a>);

impl Bucket {
    /// Gmail's real limit.
    #[must_use]
    pub fn new() -> Self {
        Self::with_rate(UNITS_PER_SECOND)
    }

    /// A bucket at an arbitrary rate. Tests use this; production does not.
    #[must_use]
    pub fn with_rate(per_second: f64) -> Self {
        let per_second = per_second.max(f64::MIN_POSITIVE);
        Self {
            state: Mutex::new(State {
                units: per_second,
                last: Instant::now(),
            }),
            capacity: per_second,
            per_second,
            in_flight: tokio::sync::Semaphore::new(MAX_CONCURRENT_REQUESTS),
        }
    }

    /// Debit `cost` units and report how long the caller must wait first.
    ///
    /// Deterministic and side-effect free apart from the debit, which is what
    /// lets the tests assert the schedule exactly instead of measuring elapsed
    /// wall time and hoping.
    pub fn reserve_at(&self, cost: u32, now: Instant) -> Duration {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| {
            // A panic inside this tiny critical section cannot leave the
            // arithmetic inconsistent, so recovering is strictly better than
            // taking the whole worker down with it.
            poisoned.into_inner()
        });

        let elapsed = now.saturating_duration_since(state.last).as_secs_f64();
        state.units = (state.units + elapsed * self.per_second).min(self.capacity);
        state.last = now;

        state.units -= f64::from(cost);
        if state.units >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-state.units / self.per_second)
        }
    }

    /// Wait until `cost` units are ours, then return.
    pub async fn acquire(&self, cost: u32) {
        let wait = self.reserve_at(cost, Instant::now());
        if !wait.is_zero() {
            tracing::debug!(cost, wait_ms = wait.as_millis(), "gmail quota: waiting");
            tokio::time::sleep(wait).await;
        }
    }

    /// Both budgets, in the only order that is safe: take the concurrency slot
    /// **first**, then the units.
    ///
    /// The other order debits units and then blocks holding the debit, which
    /// makes the recorded rate diverge from the rate Gmail actually sees.
    ///
    /// # Panics
    /// Never: the semaphore is owned by this `Bucket` and is never closed.
    pub async fn enter(&self, cost: u32) -> Slot<'_> {
        let permit = self
            .in_flight
            .acquire()
            .await
            .expect("the gmail concurrency semaphore is never closed");
        self.acquire(cost).await;
        Slot(permit)
    }

    /// Concurrency slots currently free. Test-only visibility into axis 2.
    #[must_use]
    pub fn free_slots(&self) -> usize {
        self.in_flight.available_permits()
    }
}

impl Default for Bucket {
    fn default() -> Self {
        Self::new()
    }
}

/// Exponential backoff, 1 s → 60 s, with jitter.
///
/// `jitter` is injected in `0.0..=1.0` so the schedule is testable; production
/// passes a fresh random draw. The delay lands in `[base/2, base]`, which spreads
/// a herd of retries without ever waiting longer than the cap.
///
/// A server-sent `Retry-After` wins when it is longer than our own figure -
/// Google knows more about its own throttle than we do - but is itself capped so
/// a hostile or mistaken header cannot pin a worker down for an hour.
#[must_use]
pub fn backoff(attempt: u32, retry_after: Option<Duration>, jitter: f64) -> Duration {
    let base_secs = 1u64
        .checked_shl(attempt.min(16))
        .unwrap_or(u64::MAX)
        .min(MAX_BACKOFF.as_secs());
    let jitter = jitter.clamp(0.0, 1.0);
    #[allow(clippy::cast_precision_loss)]
    let jittered = Duration::from_secs_f64(base_secs as f64 * (0.5 + 0.5 * jitter));

    match retry_after {
        Some(server) if server > jittered => server.min(MAX_RETRY_AFTER),
        _ => jittered,
    }
}

/// A fresh jitter draw.
#[must_use]
pub fn random_jitter() -> f64 {
    rand::random::<f64>()
}

/// `Retry-After` is either delta-seconds or an HTTP date; only the first form
/// appears from Google, and an unparseable value is simply absent rather than an
/// error.
#[must_use]
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Criterion L1 - the ceiling is 50 `messages.get` per second, and it is
    /// asserted against an injected clock rather than by timing anything.
    #[test]
    fn the_bucket_is_250_units_per_second() {
        let bucket = Bucket::new();
        let t0 = Instant::now();

        // A full bucket funds exactly 50 gets (50 x 5 = 250 units).
        for index in 0..50 {
            assert_eq!(
                bucket.reserve_at(cost::MESSAGES_GET, t0),
                Duration::ZERO,
                "get {index} should not have waited"
            );
        }
        // The 51st has to wait for 5 units to refill: 5/250 s = 20 ms.
        let wait = bucket.reserve_at(cost::MESSAGES_GET, t0);
        assert_eq!(wait, Duration::from_millis(20));

        // A second later the bucket has refilled - but the 51st get's debt is
        // repaid out of that refill rather than forgiven, so 49 gets are free
        // and the 50th waits again. A limiter that forgave the debt would let a
        // burst run permanently over Gmail's ceiling.
        let t1 = t0 + Duration::from_secs(1);
        for index in 0..49 {
            assert_eq!(
                bucket.reserve_at(cost::MESSAGES_GET, t1),
                Duration::ZERO,
                "get {index} in the second window should not have waited"
            );
        }
        assert_eq!(
            bucket.reserve_at(cost::MESSAGES_GET, t1),
            Duration::from_millis(20),
            "the overdraft from the first window must carry forward"
        );
    }

    /// The true cost is debited per method, not one "request" per call.
    #[test]
    fn each_method_debits_its_own_cost() {
        let bucket = Bucket::new();
        let t0 = Instant::now();
        // 250 cheap calls at 1 unit each fit in one second...
        for _ in 0..250 {
            assert_eq!(bucket.reserve_at(cost::LABELS_LIST, t0), Duration::ZERO);
        }
        assert!(bucket.reserve_at(cost::LABELS_LIST, t0) > Duration::ZERO);

        // ...but only 50 expensive ones do.
        let bucket = Bucket::new();
        for _ in 0..50 {
            assert_eq!(bucket.reserve_at(cost::MESSAGES_LIST, t0), Duration::ZERO);
        }
        assert!(bucket.reserve_at(cost::MESSAGES_LIST, t0) > Duration::ZERO);
    }

    /// Criterion L2.
    #[test]
    fn refill_is_proportional_and_clamped() {
        let bucket = Bucket::with_rate(100.0);
        let t0 = Instant::now();

        // Drain it.
        assert_eq!(bucket.reserve_at(100, t0), Duration::ZERO);
        // Half a second later, half the capacity is back: 50 units.
        assert_eq!(
            bucket.reserve_at(50, t0 + Duration::from_millis(500)),
            Duration::ZERO
        );
        // And it is empty again.
        assert!(bucket.reserve_at(1, t0 + Duration::from_millis(500)) > Duration::ZERO);

        // An idle hour does not bank an hour's worth of quota.
        let bucket = Bucket::with_rate(100.0);
        let later = t0 + Duration::from_secs(3600);
        assert_eq!(bucket.reserve_at(100, later), Duration::ZERO);
        assert!(
            bucket.reserve_at(1, later) > Duration::ZERO,
            "capacity must clamp at one second's worth"
        );
    }

    /// Criterion L3 - the classic "wait for capacity" loop deadlocks here.
    #[test]
    fn an_oversized_cost_never_deadlocks() {
        let bucket = Bucket::with_rate(10.0);
        let t0 = Instant::now();
        let wait = bucket.reserve_at(35, t0);
        // 10 in the bucket, 35 wanted: 25 units still owed at 10/s = 2.5 s.
        assert_eq!(wait, Duration::from_millis(2500));
        // And the debt is repaid rather than accumulating forever.
        assert_eq!(
            bucket.reserve_at(0, t0 + Duration::from_millis(2500)),
            Duration::ZERO
        );
    }

    /// Criterion L4.
    #[test]
    fn backoff_is_exponential_and_capped() {
        // Full jitter = the nominal schedule.
        let schedule: Vec<u64> = (0..10)
            .map(|attempt| backoff(attempt, None, 1.0).as_secs())
            .collect();
        assert_eq!(schedule, vec![1, 2, 4, 8, 16, 32, 60, 60, 60, 60]);

        // Half jitter halves it, and never below half.
        for attempt in 0..10 {
            let low = backoff(attempt, None, 0.0);
            let high = backoff(attempt, None, 1.0);
            assert_eq!(low * 2, high, "attempt {attempt}");
            assert!(high <= MAX_BACKOFF, "attempt {attempt} exceeded the cap");
            assert!(low >= Duration::from_millis(500));
        }

        // A real draw stays inside the band.
        for attempt in 0..8 {
            let delay = backoff(attempt, None, random_jitter());
            assert!(delay <= MAX_BACKOFF && delay >= Duration::from_millis(500));
        }
    }

    /// Criterion L6.
    #[test]
    fn retry_after_wins_when_longer() {
        // Longer than ours: honoured.
        assert_eq!(
            backoff(0, Some(Duration::from_secs(30)), 1.0),
            Duration::from_secs(30)
        );
        // Shorter than ours: ignored, because it would retry into the throttle.
        assert_eq!(
            backoff(5, Some(Duration::from_secs(1)), 1.0),
            Duration::from_secs(32)
        );
        // Absurd: capped, so a bad header cannot pin a worker down.
        assert_eq!(
            backoff(0, Some(Duration::from_secs(86_400)), 1.0),
            MAX_RETRY_AFTER
        );

        assert_eq!(parse_retry_after("42"), Some(Duration::from_secs(42)));
        assert_eq!(parse_retry_after(" 7 "), Some(Duration::from_secs(7)));
        // EDGE: an HTTP-date form, or nonsense, is absent rather than an error.
        assert_eq!(parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after(""), None);
    }

    /// The axis the unit bucket cannot express, and the one the live sync was
    /// actually throttled on: `"Too many concurrent requests for user."`
    ///
    /// A rate budget with capacity to spare still says yes to two simultaneous
    /// callers, which is exactly what Gmail says no to.
    #[tokio::test]
    async fn a_second_caller_waits_for_the_concurrency_slot() {
        let bucket = std::sync::Arc::new(Bucket::new());
        assert_eq!(bucket.free_slots(), MAX_CONCURRENT_REQUESTS);

        // One cheap call: the *rate* budget is barely touched...
        let held = bucket.enter(cost::LABELS_LIST).await;
        assert_eq!(bucket.free_slots(), 0, "the slot is taken while in flight");

        // ...and a second caller is nonetheless made to wait, because the
        // limit it would breach is a count and not a rate.
        let second = tokio::spawn({
            let bucket = std::sync::Arc::clone(&bucket);
            async move {
                let _slot = bucket.enter(cost::LABELS_LIST).await;
                true
            }
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), async {})
                .await
                .is_ok()
                && !second.is_finished(),
            "a second caller must not proceed while a request is in flight"
        );

        drop(held);
        assert!(tokio::time::timeout(Duration::from_secs(5), second)
            .await
            .expect("the second caller must proceed once the slot frees")
            .unwrap());
        assert_eq!(bucket.free_slots(), MAX_CONCURRENT_REQUESTS);
    }

    /// A batch is `n` simultaneous operations to Google, so the batch width is
    /// a concurrency figure and has to sit inside the concurrency budget - not
    /// merely inside the unit budget, which is what 45 was chosen against.
    #[test]
    fn the_batch_width_is_sized_against_concurrency_not_units() {
        const {
            assert!(
                MAX_CONCURRENT_SUBREQUESTS <= 10,
                "45 sub-requests produced `Too many concurrent requests for user` live"
            );
        }
        // And it is still comfortably inside the rate ceiling, so fixing one
        // axis did not break the other.
        let units = f64::from(cost::MESSAGES_GET)
            * u32::try_from(MAX_CONCURRENT_SUBREQUESTS)
                .map(f64::from)
                .unwrap_or(f64::MAX);
        assert!(
            units <= UNITS_PER_SECOND / 2.0,
            "a batch costs {units} units of a {UNITS_PER_SECOND}/s ceiling"
        );
    }

    /// Criterion L7 - concurrent callers share one schedule and none of them
    /// gets a free ride.
    #[tokio::test]
    async fn concurrent_callers_share_the_budget() {
        let bucket = std::sync::Arc::new(Bucket::new());
        let t0 = Instant::now();
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let bucket = std::sync::Arc::clone(&bucket);
                tokio::spawn(async move {
                    let mut free = 0usize;
                    for _ in 0..10 {
                        if bucket.reserve_at(cost::MESSAGES_GET, t0) == Duration::ZERO {
                            free += 1;
                        }
                    }
                    free
                })
            })
            .collect();

        let mut free = 0usize;
        for task in tasks {
            free += task.await.unwrap();
        }
        // 80 gets requested at one instant; exactly 250/5 = 50 are free.
        assert_eq!(free, 50);
    }
}
