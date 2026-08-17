//! Time, and only the time a test gave us.
//!
//! Determinism is a hard requirement of this crate, so the response path never
//! reads a wall clock. Everything that needs "now" — `internalDate` on an
//! inserted message, a `watch` expiry, the quota bucket's refill, a
//! `newer_than:` boundary — goes through [`TestClock`], which moves only when a
//! test calls [`TestClock::advance`] or [`TestClock::set_ms`].
//!
//! The half of this rule that a reviewer cannot forget to enforce is in
//! `Cargo.toml`: `chrono` is taken with `default-features = false`, so
//! `chrono::Utc::now` is not compiled into the dependency graph at all.

use std::{
    fmt,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    },
    time::Duration,
};

/// Midnight UTC on 2026-09-01, in epoch milliseconds.
///
/// An arbitrary but fixed default, chosen so that the live `.eml` sample (mid
/// August 2026) sits comfortably inside a 30-day window ending here. Tests that
/// care about absolute time should set the clock explicitly rather than lean on
/// this.
pub const DEFAULT_NOW_MS: i64 = 1_788_220_800_000;

/// A source of "now", in epoch milliseconds.
///
/// A client that wants to share the simulator's notion of time can be generic
/// over this rather than reaching for `SystemTime::now`.
pub trait Clock: Send + Sync + fmt::Debug {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> i64;
}

/// The only clock this crate ships: a shared counter a test advances by hand.
///
/// Cloning shares the underlying instant, so a test can hold one handle while
/// the simulator holds another.
///
/// ```
/// use std::time::Duration;
/// use nade_gmail_sim::{Clock, TestClock};
///
/// let clock = TestClock::at_ms(1_000);
/// let handle = clock.clone();
/// clock.advance(Duration::from_secs(2));
/// assert_eq!(handle.now_ms(), 3_000);
/// ```
#[derive(Clone)]
pub struct TestClock(Arc<AtomicI64>);

impl TestClock {
    /// A clock parked at [`DEFAULT_NOW_MS`].
    #[must_use]
    pub fn new() -> Self {
        Self::at_ms(DEFAULT_NOW_MS)
    }

    /// A clock parked at `ms` epoch milliseconds.
    #[must_use]
    pub fn at_ms(ms: i64) -> Self {
        Self(Arc::new(AtomicI64::new(ms)))
    }

    /// Milliseconds since the Unix epoch.
    #[must_use]
    pub fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }

    /// Move time forward.
    ///
    /// Saturates at [`i64::MAX`] rather than wrapping: a test that asks for a
    /// century should get a very large timestamp, not a negative one.
    ///
    /// EDGE: absurd durations. `Duration::MAX.as_millis()` does not fit in an
    /// `i64`, and an overflowing add would silently produce a time *before* the
    /// mailbox was seeded, which reads as clock skew rather than as the test's
    /// own mistake.
    pub fn advance(&self, by: Duration) {
        let millis = i64::try_from(by.as_millis()).unwrap_or(i64::MAX);
        self.advance_ms(millis);
    }

    /// Move time forward by whole milliseconds. Saturating; see [`Self::advance`].
    pub fn advance_ms(&self, ms: i64) {
        let _ = self
            .0
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |now| {
                Some(now.saturating_add(ms))
            });
    }

    /// Jump to an absolute epoch-millisecond instant, forwards or backwards.
    ///
    /// Going backwards is allowed on purpose: clock skew is a thing a sync
    /// client has to survive, and refusing to model it would hide that.
    pub fn set_ms(&self, ms: i64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl Default for TestClock {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for TestClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("TestClock").field(&self.now_ms()).finish()
    }
}

impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        Self::now_ms(self)
    }
}

/// Milliseconds in a day, for `newer_than:`/`older_than:` arithmetic.
pub const DAY_MS: i64 = 86_400_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_now_is_the_first_of_september_2026() {
        // Cross-checked against `date -u -r 1788220800` rather than computed by
        // the same arithmetic that produced the constant.
        assert_eq!(DEFAULT_NOW_MS % 1000, 0);
        assert_eq!(DEFAULT_NOW_MS / 1000, 1_788_220_800);
        assert_eq!(
            crate::message::rfc2822(DEFAULT_NOW_MS),
            "Tue, 01 Sep 2026 00:00:00 +0000"
        );
    }

    #[test]
    fn clones_share_one_instant() {
        let a = TestClock::at_ms(0);
        let b = a.clone();
        a.advance_ms(5);
        b.advance_ms(7);
        assert_eq!(a.now_ms(), 12);
        assert_eq!(b.now_ms(), 12);
    }

    #[test]
    fn advancing_saturates_instead_of_wrapping() {
        let clock = TestClock::at_ms(i64::MAX - 1);
        clock.advance(Duration::from_secs(u64::from(u32::MAX)));
        assert_eq!(clock.now_ms(), i64::MAX);
        clock.advance(Duration::MAX);
        assert_eq!(clock.now_ms(), i64::MAX);
    }

    #[test]
    fn time_can_be_set_backwards() {
        let clock = TestClock::at_ms(1_000);
        clock.set_ms(10);
        assert_eq!(clock.now_ms(), 10);
    }
}
