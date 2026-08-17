//! The one-time pairing code and the device bearer token.
//!
//! PLAN.md §Auth bootstrap: "Server startup prints a one-time 6-digit pairing
//! code (also `just pair` reprints). Code is single-use, 10-min TTL. Tokens are
//! opaque 32-byte, hashed at rest, revocable."
//!
//! The live code is held in memory *and* mirrored to a 0600 file under
//! `backend/secrets/` (gitignored). That file is what makes `just pair` - a
//! second, separate process - able to reprint or replace the running server's
//! code at all; see backend/DECISIONS.md D4 for why that trade is acceptable.
//! It also buys atomic single-use: `unlink` has exactly one winner, so two
//! racing pair requests can never both succeed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use rand::TryRngCore;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::sync::RwLock;

use crate::config::PairingConfig;

/// Wire prefix for device bearer tokens, per `docs/contract/pair.json`.
pub const TOKEN_PREFIX: &str = "nade_";
/// Entropy behind a token. 32 bytes -> 64 hex characters.
pub const TOKEN_BYTES: usize = 32;
/// Nobody's pairing code is longer than this; anything else is rejected before
/// we do any work on it.
const MAX_PRESENTED_CODE_LEN: usize = 32;
/// How far the wall clock may run backwards before we distrust a stored code.
const CLOCK_SKEW_TOLERANCE_SECS: i64 = 60;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingCode {
    pub code: String,
    pub minted_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl PairingCode {
    #[must_use]
    pub fn is_live(&self, now: DateTime<Utc>) -> bool {
        self.expires_at > now
    }

    /// Time left, saturating at zero.
    #[must_use]
    pub fn remaining(&self, now: DateTime<Utc>) -> Duration {
        (self.expires_at - now).to_std().unwrap_or(Duration::ZERO)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consumed {
    Accepted,
    Rejected,
}

#[derive(Debug)]
pub struct PairingStore {
    path: PathBuf,
    ttl: Duration,
    /// Write-through cache of the last code this process minted. The file is
    /// the authority - always re-read before a decision - so a code minted by
    /// `just pair` in another process is honoured immediately.
    last: RwLock<Option<PairingCode>>,
}

impl PairingStore {
    #[must_use]
    pub fn new(config: &PairingConfig) -> Self {
        Self {
            path: config.state_file.clone(),
            ttl: config.ttl,
            last: RwLock::new(None),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Mint a fresh code, replacing any live one.
    ///
    /// # Errors
    /// Returns an error if the OS randomness or the state file is unavailable.
    pub async fn mint(&self) -> Result<PairingCode> {
        let now = Utc::now();
        let code = PairingCode {
            code: random_code()?,
            minted_at: now,
            expires_at: now
                + chrono::Duration::from_std(self.ttl).context("pairing TTL is out of range")?,
        };
        self.write(&code).await?;
        *self.last.write().await = Some(code.clone());
        Ok(code)
    }

    /// The live code, if there is one. Used by `nade-server --print-pair-code`.
    ///
    /// # Errors
    /// Returns an error if the state file cannot be read.
    pub async fn current(&self) -> Result<Option<PairingCode>> {
        let now = Utc::now();
        Ok(self.read().await?.filter(|code| code.is_live(now)))
    }

    /// Check a presented code and, if it matches, spend it.
    ///
    /// Never returns a reason: a caller learns only accepted/rejected, so a
    /// prober cannot tell "wrong" from "expired" from "already used".
    pub async fn consume(&self, presented: &str) -> Consumed {
        // EDGE (empty input / oversized input): both are rejected before any
        // comparison work happens.
        if presented.is_empty() || presented.len() > MAX_PRESENTED_CODE_LEN {
            return Consumed::Rejected;
        }

        let Ok(Some(stored)) = self.read().await else {
            return Consumed::Rejected;
        };

        let now = Utc::now();
        // EDGE (clock skew): a code stamped in the future means the wall clock
        // moved backwards after it was minted. Its expiry is meaningless now,
        // so drop it rather than honour it for an unknown length of time.
        if stored.minted_at - now > chrono::Duration::seconds(CLOCK_SKEW_TOLERANCE_SECS) {
            tracing::warn!("pairing code was minted in the future; discarding it");
            self.forget().await;
            return Consumed::Rejected;
        }

        // EDGE (expiry).
        if !stored.is_live(now) {
            self.forget().await;
            return Consumed::Rejected;
        }

        // Constant time, and safe for mismatched lengths (`subtle` returns
        // false immediately when the slices differ in length).
        if !bool::from(stored.code.as_bytes().ct_eq(presented.as_bytes())) {
            return Consumed::Rejected;
        }

        // EDGE (duplicate delivery / replay): single-use enforced by `unlink`,
        // which exactly one caller can win - even across processes.
        match tokio::fs::remove_file(&self.path).await {
            Ok(()) => {
                *self.last.write().await = None;
                Consumed::Accepted
            }
            Err(_) => Consumed::Rejected,
        }
    }

    /// Drop the live code without pairing.
    pub async fn forget(&self) {
        let _ = tokio::fs::remove_file(&self.path).await;
        *self.last.write().await = None;
    }

    async fn read(&self) -> Result<Option<PairingCode>> {
        match tokio::fs::read(&self.path).await {
            Ok(bytes) => match serde_json::from_slice::<PairingCode>(&bytes) {
                Ok(code) => Ok(Some(code)),
                // EDGE: a truncated or hand-edited state file must not wedge
                // pairing; treat it as "no live code".
                Err(error) => {
                    tracing::warn!(path = %self.path.display(), %error, "unreadable pairing state");
                    Ok(None)
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(anyhow!(error).context(format!("reading {}", self.path.display()))),
        }
    }

    async fn write(&self, code: &PairingCode) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("creating {}", parent.display()))?;
            restrict(parent, 0o700);
        }
        // Write-then-rename so a reader never sees half a file.
        let temporary = self.path.with_extension("tmp");
        tokio::fs::write(&temporary, serde_json::to_vec(code)?)
            .await
            .with_context(|| format!("writing {}", temporary.display()))?;
        restrict(&temporary, 0o600);
        tokio::fs::rename(&temporary, &self.path)
            .await
            .with_context(|| format!("installing {}", self.path.display()))?;
        Ok(())
    }
}

fn restrict(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

/// A uniformly distributed 6-digit code.
///
/// Rejection sampling, not `% 1_000_000`: the naive modulo would make the
/// 967 295 codes below 967 296 very slightly likelier than the rest, and a
/// brute-forcer would start there.
///
/// # Errors
/// Returns an error if the OS randomness source is unavailable.
pub fn random_code() -> Result<String> {
    const CODES: u32 = 1_000_000;
    /// The largest whole multiple of `CODES` inside `u32`. Draws at or above
    /// it are thrown away, which is what removes the bias.
    const LIMIT: u32 = (u32::MAX / CODES) * CODES;

    let mut buffer = [0u8; 4];
    loop {
        fill(&mut buffer)?;
        let draw = u32::from_le_bytes(buffer);
        if draw < LIMIT {
            return Ok(format!("{:06}", draw % CODES));
        }
    }
}

/// A device bearer token: `nade_` + 64 hex characters of OS randomness.
///
/// # Errors
/// Returns an error if the OS randomness source is unavailable.
pub fn random_token() -> Result<String> {
    let mut buffer = [0u8; TOKEN_BYTES];
    fill(&mut buffer)?;
    Ok(format!("{TOKEN_PREFIX}{}", hex::encode(buffer)))
}

/// What we store in `devices.bearer_hash`. The plaintext token is handed to the
/// device once and never written down.
#[must_use]
pub fn token_hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn fill(buffer: &mut [u8]) -> Result<()> {
    rand::rngs::OsRng
        .try_fill_bytes(buffer)
        .map_err(|error| anyhow!("the OS randomness source is unavailable: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(ttl: Duration) -> PairingStore {
        PairingStore::new(&PairingConfig {
            state_file: std::env::temp_dir()
                .join(format!("nade-pair-{}.json", uuid::Uuid::new_v4())),
            ttl,
            rate_limit: 10,
            rate_window: Duration::from_secs(60),
        })
    }

    /// Criterion F1.
    #[test]
    fn pairing_codes_are_six_digits_and_unbiased() {
        const DRAWS: usize = 20_000;
        let mut last_digit = [0usize; 10];
        let mut distinct = std::collections::HashSet::new();

        for _ in 0..DRAWS {
            let code = random_code().unwrap();
            assert_eq!(code.len(), 6, "{code}");
            assert!(code.bytes().all(|b| b.is_ascii_digit()), "{code}");
            let digit = usize::from(code.as_bytes()[5] - b'0');
            last_digit[digit] += 1;
            distinct.insert(code);
        }

        // Expected 2000 per bucket, sd ~42. A +/-500 band is ~12 sd wide: this
        // catches a broken generator without ever flaking on a working one.
        for (digit, count) in last_digit.iter().enumerate() {
            assert!(
                (1500..=2500).contains(count),
                "digit {digit} appeared {count} times in {DRAWS} draws"
            );
        }
        assert!(
            distinct.len() > DRAWS * 9 / 10,
            "only {} distinct codes in {DRAWS} draws",
            distinct.len()
        );
    }

    #[test]
    fn tokens_are_a_prefixed_hex_of_thirty_two_bytes() {
        let token = random_token().unwrap();
        let hex = token.strip_prefix(TOKEN_PREFIX).expect("nade_ prefix");
        assert_eq!(hex.len(), TOKEN_BYTES * 2);
        assert!(hex
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_ne!(token, random_token().unwrap());
    }

    #[test]
    fn token_hash_is_sha256_hex_of_the_whole_token() {
        // Fixed vectors, so changing the hashing scheme has to be a deliberate
        // act rather than an accident (it would strand every paired device).
        assert_eq!(
            token_hash("nade_"),
            "2d0ebec687628ce9aec431a77b93e56cd1fcc8400d3b29c9bbe22d7aed2805a5"
        );
        assert_eq!(
            token_hash("nade_deadbeef"),
            "70e236fb5add14471f664856c454aa8e6eba37a4ef0e996b30315ebbed2f11a6"
        );
        assert_ne!(token_hash("nade_deadbeef"), token_hash("nade_deadbeee"));
    }

    #[tokio::test]
    async fn consume_is_single_use() {
        let store = store(Duration::from_secs(600));
        let code = store.mint().await.unwrap();
        assert_eq!(store.consume(&code.code).await, Consumed::Accepted);
        assert_eq!(store.consume(&code.code).await, Consumed::Rejected);
        assert!(store.current().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn minting_replaces_the_previous_code() {
        let store = store(Duration::from_secs(600));
        let first = store.mint().await.unwrap();
        let second = store.mint().await.unwrap();
        assert_eq!(store.consume(&first.code).await, Consumed::Rejected);
        assert_eq!(store.consume(&second.code).await, Consumed::Accepted);
    }

    #[tokio::test]
    async fn an_expired_code_is_rejected_by_the_store() {
        let store = store(Duration::from_millis(1));
        let code = store.mint().await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(store.consume(&code.code).await, Consumed::Rejected);
    }

    /// Edge case 8 (clock skew).
    #[tokio::test]
    async fn a_code_minted_in_the_future_is_rejected() {
        let store = store(Duration::from_secs(600));
        let code = store.mint().await.unwrap();
        let skewed = PairingCode {
            minted_at: Utc::now() + chrono::Duration::hours(3),
            expires_at: Utc::now() + chrono::Duration::hours(4),
            ..code.clone()
        };
        store.write(&skewed).await.unwrap();
        assert_eq!(store.consume(&code.code).await, Consumed::Rejected);
    }

    /// Edge case 1 (empty input) and 2 (unicode).
    #[tokio::test]
    async fn junk_codes_are_rejected_without_spending_the_real_one() {
        let store = store(Duration::from_secs(600));
        let code = store.mint().await.unwrap();
        for junk in [
            "",
            "0",
            "🔐🔐🔐",
            "000000000000000000000000000000000000",
            "abcdef",
        ] {
            assert_eq!(store.consume(junk).await, Consumed::Rejected, "{junk:?}");
        }
        assert_eq!(store.consume(&code.code).await, Consumed::Accepted);
    }

    #[tokio::test]
    async fn a_corrupt_state_file_reads_as_no_code() {
        let store = store(Duration::from_secs(600));
        store.mint().await.unwrap();
        tokio::fs::write(store.path(), b"{not json").await.unwrap();
        assert!(store.current().await.unwrap().is_none());
        assert_eq!(store.consume("123456").await, Consumed::Rejected);
    }
}
