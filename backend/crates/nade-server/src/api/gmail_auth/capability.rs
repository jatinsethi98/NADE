//! The capability that authorises *initiating* a Gmail link.
//!
//! `GET /v1/auth/gmail/start` is browser-facing and cannot carry a bearer token,
//! but "cannot carry a bearer token" is not the same as "anyone may call it".
//! Before this module existed, a reachable, not-yet-connected server would let
//! **any** caller walk the consent flow with their own Google account and become
//! the singleton mailbox; the real owner then met the 409.
//!
//! So authorisation to start moves to a route that *can* be authenticated:
//! `POST /v1/auth/gmail/link` mints one of these behind the bearer guard and
//! hands back the full `start` URL carrying it. Pairing is the trust root - the
//! pairing code is printed on the server's own console, so a device holding a
//! bearer token has already proved console access - and there is therefore no
//! bootstrapping problem.
//!
//! The discipline matches [`crate::gmail::oauth::PendingAuths`]: bounded,
//! expiring, single-use, and compared in constant time.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use rand::TryRngCore;
use subtle::ConstantTimeEq;
use uuid::Uuid;

/// How long a minted capability stays usable, in seconds.
///
/// The same ten minutes as the pairing code and the OAuth `state`, so a human
/// has one budget to remember rather than three.
pub const CAPABILITY_TTL_SECS: u64 = 600;

/// [`CAPABILITY_TTL_SECS`] as a [`Duration`].
pub const CAPABILITY_TTL: Duration = Duration::from_secs(CAPABILITY_TTL_SECS);

/// Beyond this many live capabilities the oldest is dropped. A person taps
/// "Connect Gmail" once, maybe twice; this is a memory bound, not a policy.
const MAX_CAPABILITIES: usize = 16;

/// 32 bytes of OS randomness - 64 hex characters, the same strength as a device
/// bearer token's body.
const SECRET_BYTES: usize = 32;

/// Nothing longer than this can be a capability, so it is refused before any
/// comparison work happens.
const MAX_PRESENTED_LEN: usize = 128;

/// Deliberately **not** `Debug`: the token is a live credential, and a derived
/// `Debug` would put it in any log line that ever formatted the store.
struct Entry {
    token: String,
    /// The device that asked for it. `None` is the `NADE_TOKEN` dev shortcut,
    /// which is a synthetic device with no `devices` row to revoke.
    device_id: Option<Uuid>,
    born: Instant,
}

/// What consuming a capability proves: a paired device asked for this link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    pub device_id: Option<Uuid>,
}

/// Live capabilities: bounded, expiring, single-use.
#[derive(Default)]
pub struct LinkCapabilities {
    entries: Mutex<Vec<Entry>>,
}

/// Hand-written, like [`crate::state::AppState`]'s, so the count is printable
/// and the tokens are not.
impl std::fmt::Debug for LinkCapabilities {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `try_lock`, not `lock`: a `Debug` impl that can deadlock is a landmine,
        // and "cannot say right now" is a perfectly good answer here.
        match self.entries.try_lock() {
            Ok(entries) => f
                .debug_struct("LinkCapabilities")
                .field("live", &entries.len())
                .finish_non_exhaustive(),
            Err(_) => f.write_str("LinkCapabilities { .. }"),
        }
    }
}

impl LinkCapabilities {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a capability on behalf of `device_id`.
    ///
    /// # Errors
    /// Returns an error if the OS randomness source is unavailable.
    pub fn mint(&self, device_id: Option<Uuid>) -> Result<String> {
        let token = random_secret()?;
        self.insert(Entry {
            token: token.clone(),
            device_id,
            born: Instant::now(),
        });
        Ok(token)
    }

    fn insert(&self, entry: Entry) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        entries.retain(|live| !expired(live.born, now));
        // EDGE (store at capacity): drop the oldest rather than refuse a
        // legitimate new link. Entries are pushed in mint order, so index 0 is
        // always the oldest.
        while entries.len() >= MAX_CAPABILITIES {
            entries.remove(0);
        }
        entries.push(entry);
    }

    /// Verify and consume. `None` when the token is absent, malformed, unknown,
    /// already spent, or expired - deliberately indistinguishable to the caller,
    /// because `start` must render one refusal page for all of them.
    pub fn take(&self, presented: &str) -> Option<Capability> {
        // EDGE (empty input / oversized input): both refused before any
        // comparison work, so a megabyte of query string costs nothing.
        if presented.is_empty() || presented.len() > MAX_PRESENTED_LEN {
            return None;
        }

        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        // EDGE (expiry): pruned on the way in, so an expired token can never
        // match even if it is still sitting in the vector.
        entries.retain(|live| !expired(live.born, now));

        // Constant-time, and the scan deliberately does **not** stop early: the
        // cost of a call must not tell a caller where in the set a near-miss
        // landed. Tokens are 32 random bytes, so at most one can ever match.
        let mut hit: Option<usize> = None;
        for (index, entry) in entries.iter().enumerate() {
            if bool::from(entry.token.as_bytes().ct_eq(presented.as_bytes())) {
                hit = Some(index);
            }
        }

        // EDGE (replay / duplicate delivery): single use. The entry is gone
        // whether or not the caller goes on to succeed, so a second `start`
        // with the same `cap` is refused exactly like an unknown one.
        let entry = entries.remove(hit?);
        Some(Capability {
            device_id: entry.device_id,
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Insert a capability with a chosen birth instant.
    ///
    /// The only way to test the ten-minute TTL without a ten-minute test.
    #[cfg(test)]
    pub fn remember_at(&self, token: String, device_id: Option<Uuid>, born: Instant) {
        self.insert(Entry {
            token,
            device_id,
            born,
        });
    }
}

/// EDGE (clock running backwards): [`Instant`] is monotonic, so moving the wall
/// clock cannot extend or shorten a TTL. `saturating_duration_since` keeps an
/// entry stamped in the future (reachable only from the test seam, or from a
/// platform whose monotonic clock is not) alive rather than panicking.
fn expired(born: Instant, now: Instant) -> bool {
    now.saturating_duration_since(born) >= CAPABILITY_TTL
}

/// 32 bytes of OS randomness as lowercase hex.
///
/// Used for the capability itself **and** for the browser-binding cookie value,
/// which needs the same unguessability and the same URL/header safety.
///
/// # Errors
/// Returns an error if the OS randomness source is unavailable.
pub fn random_secret() -> Result<String> {
    let mut buffer = [0u8; SECRET_BYTES];
    rand::rngs::OsRng
        .try_fill_bytes(&mut buffer)
        .map_err(|error| anyhow!("the OS randomness source is unavailable: {error}"))?;
    Ok(hex::encode(buffer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capability_is_single_use() {
        let store = LinkCapabilities::new();
        let device = Uuid::new_v4();
        let token = store.mint(Some(device)).unwrap();

        assert_eq!(
            store.take(&token),
            Some(Capability {
                device_id: Some(device)
            })
        );
        // EDGE (replay): a second use is refused, and is indistinguishable from
        // a token we never issued.
        assert_eq!(store.take(&token), None);
        assert_eq!(store.take("never-issued"), None);
        assert!(store.is_empty());
    }

    /// EDGE (expiry) and EDGE (clock running backwards) in one place - both are
    /// reachable only by choosing the birth instant.
    #[test]
    fn an_expired_capability_is_refused_and_a_future_one_is_not() {
        let store = LinkCapabilities::new();

        // A birth instant in the future cannot arise from a monotonic clock; if
        // one ever did, it must not panic and must not be treated as expired.
        let future = random_secret().unwrap();
        store.remember_at(
            future.clone(),
            None,
            Instant::now() + Duration::from_secs(3_600),
        );
        assert!(store.take(&future).is_some());

        // The monotonic clock is measured from boot on some platforms, so a
        // machine that came up seconds ago cannot express "eleven minutes ago".
        // Skipping beats a test that flakes once a month.
        let Some(long_ago) = Instant::now().checked_sub(CAPABILITY_TTL + Duration::from_secs(1))
        else {
            println!("skipped: the monotonic clock has not been running for ten minutes");
            return;
        };

        let stale = random_secret().unwrap();
        store.remember_at(stale.clone(), None, long_ago);
        assert_eq!(store.take(&stale), None, "ten minutes is the whole budget");

        // Exactly at the boundary is expired too: `>=`, not `>`.
        let edge = random_secret().unwrap();
        store.remember_at(
            edge.clone(),
            None,
            Instant::now().checked_sub(CAPABILITY_TTL).unwrap(),
        );
        assert_eq!(store.take(&edge), None);
    }

    #[test]
    fn capabilities_are_bounded() {
        let store = LinkCapabilities::new();
        let mut tokens = Vec::new();
        for _ in 0..(MAX_CAPABILITIES * 3) {
            tokens.push(store.mint(None).unwrap());
        }
        assert!(
            store.len() <= MAX_CAPABILITIES,
            "the capability set must not grow without bound: {}",
            store.len()
        );
        // The oldest went, the newest stayed: a device that just tapped Connect
        // is never the one that loses.
        assert_eq!(store.take(&tokens[0]), None);
        assert!(store.take(tokens.last().unwrap()).is_some());
    }

    /// EDGE (empty input / unicode / oversized input).
    #[test]
    fn junk_is_refused_without_spending_a_real_capability() {
        let store = LinkCapabilities::new();
        let token = store.mint(None).unwrap();

        for junk in [
            "",
            " ",
            "0",
            "🔐🔐🔐",
            "../../etc/passwd",
            &"a".repeat(MAX_PRESENTED_LEN + 1),
            &token[..token.len() - 1], // one character short of the real one
        ] {
            assert_eq!(store.take(junk), None, "{junk:?}");
        }
        assert!(store.take(&token).is_some(), "the real one still works");
    }

    #[test]
    fn secrets_are_unguessable_and_url_safe() {
        let first = random_secret().unwrap();
        assert_eq!(first.len(), SECRET_BYTES * 2);
        assert!(
            first
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "{first}"
        );
        // Hex needs no percent-encoding in a query string and no quoting in a
        // Set-Cookie value, which is why it is hex and not base64.
        assert_ne!(first, random_secret().unwrap());
    }
}
