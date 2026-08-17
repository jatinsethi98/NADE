//! Encryption at rest for the Gmail refresh token.
//!
//! `migrations/0001_init.sql` says `gmail_tokens.access_token` holds "AES-GCM
//! ciphertext, rewritten on EVERY refresh (P2)". This is that.
//!
//! The refresh token is the whole account: it mints access tokens forever until
//! the user revokes it. A `pg_dump` in a backup bucket, or a stray `select * from
//! gmail_tokens` in a log, should not be enough to read someone's mail.
//!
//! The key lives outside the database - env var first, otherwise a 0600 file in
//! `backend/secrets/`, generated on first use. Same shape as the pairing-code
//! mirror (backend/DECISIONS.md D4): a real secret, with real file permissions,
//! in an already-gitignored directory.

use std::path::{Path, PathBuf};

use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    AeadCore, Aes256Gcm, Key, Nonce,
};
use anyhow::{bail, Context, Result};
use base64::Engine as _;

const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

/// AES-256-GCM over short secrets.
#[derive(Clone)]
pub struct Cipher {
    key: Key<Aes256Gcm>,
}

/// Never derive `Debug` on something holding a key.
impl std::fmt::Debug for Cipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cipher(<aes-256-gcm key>)")
    }
}

impl Cipher {
    #[must_use]
    pub fn from_key(key: [u8; KEY_LEN]) -> Self {
        Self {
            key: *Key::<Aes256Gcm>::from_slice(&key),
        }
    }

    /// Resolve the key: `NADE_TOKEN_KEY` (64 hex characters) if set, otherwise
    /// the key file, generating it if it does not exist yet.
    ///
    /// # Errors
    /// Returns an error if the env var is malformed, or the key file cannot be
    /// read or written.
    pub fn resolve(env_key: Option<&str>, key_file: &Path) -> Result<Self> {
        if let Some(hex_key) = env_key {
            let bytes = hex::decode(hex_key.trim())
                .context("NADE_TOKEN_KEY must be hex (64 characters = 32 bytes)")?;
            let key: [u8; KEY_LEN] = bytes.try_into().map_err(|raw: Vec<u8>| {
                anyhow::anyhow!(
                    "NADE_TOKEN_KEY must decode to {KEY_LEN} bytes, got {}",
                    raw.len()
                )
            })?;
            return Ok(Self::from_key(key));
        }
        Self::from_file(key_file)
    }

    fn from_file(path: &Path) -> Result<Self> {
        if let Ok(existing) = std::fs::read_to_string(path) {
            let bytes = hex::decode(existing.trim())
                .with_context(|| format!("{} is not hex", path.display()))?;
            let key: [u8; KEY_LEN] = bytes.try_into().map_err(|raw: Vec<u8>| {
                anyhow::anyhow!(
                    "{} holds {} bytes, expected {KEY_LEN}. Delete it to mint a fresh key - \
                     but every stored Gmail token becomes unreadable and the account must \
                     re-consent.",
                    path.display(),
                    raw.len()
                )
            })?;
            return Ok(Self::from_key(key));
        }

        let key = Aes256Gcm::generate_key(OsRng);
        let mut bytes = [0u8; KEY_LEN];
        bytes.copy_from_slice(key.as_slice());

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(path, hex::encode(bytes))
            .with_context(|| format!("writing {}", path.display()))?;
        harden(path)?;
        tracing::info!(path = %path.display(), "minted a new Gmail token encryption key");
        Ok(Self::from_key(bytes))
    }

    /// `base64url(nonce || ciphertext)`.
    ///
    /// # Errors
    /// Returns an error only if the AEAD itself fails, which in practice means
    /// an allocation failure.
    pub fn encrypt(&self, plaintext: &str) -> Result<String> {
        let cipher = Aes256Gcm::new(&self.key);
        let nonce = Aes256Gcm::generate_nonce(OsRng);
        let mut sealed = cipher
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|error| anyhow::anyhow!("encrypting a gmail token: {error}"))?;

        let mut out = Vec::with_capacity(NONCE_LEN + sealed.len());
        out.extend_from_slice(nonce.as_slice());
        out.append(&mut sealed);
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(out))
    }

    /// # Errors
    /// Returns an error if the value is not our ciphertext, or was written with
    /// a different key.
    pub fn decrypt(&self, sealed: &str) -> Result<String> {
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(sealed.trim())
            .context("a stored gmail token is not base64url")?;
        if raw.len() <= NONCE_LEN {
            bail!("a stored gmail token is truncated");
        }
        let (nonce, body) = raw.split_at(NONCE_LEN);
        let cipher = Aes256Gcm::new(&self.key);
        let plaintext = cipher
            .decrypt(Nonce::from_slice(nonce), body)
            .map_err(|_| {
                anyhow::anyhow!(
                    "a stored gmail token could not be decrypted - the key changed, so the \
                     account must re-consent"
                )
            })?;
        String::from_utf8(plaintext).context("a decrypted gmail token is not UTF-8")
    }
}

#[cfg(unix)]
fn harden(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("setting 0600 on {}", path.display()))
}

#[cfg(not(unix))]
fn harden(_path: &Path) -> Result<()> {
    Ok(())
}

/// Default location: `backend/secrets/token-key`, beside the pairing-code
/// mirror, in the same gitignored directory.
#[must_use]
pub fn default_key_file(backend_root: &Path) -> PathBuf {
    backend_root.join("secrets").join("token-key")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("nade-{name}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn round_trips_and_hides_the_plaintext() {
        let cipher = Cipher::from_key([7u8; KEY_LEN]);
        let secret = "1//0gRefreshTokenWith🚀AndÜmlauts";
        let sealed = cipher.encrypt(secret).unwrap();

        assert!(!sealed.contains("Refresh"), "{sealed}");
        assert!(!sealed.contains("1//0g"), "{sealed}");
        assert_eq!(cipher.decrypt(&sealed).unwrap(), secret);
    }

    #[test]
    fn the_nonce_is_fresh_every_time() {
        let cipher = Cipher::from_key([1u8; KEY_LEN]);
        let a = cipher.encrypt("same").unwrap();
        let b = cipher.encrypt("same").unwrap();
        assert_ne!(a, b, "a repeated nonce would leak equality of plaintexts");
        assert_eq!(cipher.decrypt(&a).unwrap(), cipher.decrypt(&b).unwrap());
    }

    #[test]
    fn a_wrong_key_or_tampered_ciphertext_fails_closed() {
        let cipher = Cipher::from_key([1u8; KEY_LEN]);
        let other = Cipher::from_key([2u8; KEY_LEN]);
        let sealed = cipher.encrypt("secret").unwrap();

        assert!(other.decrypt(&sealed).is_err(), "a wrong key must not work");

        // Flip one character of the ciphertext: GCM refuses it.
        let mut tampered: Vec<char> = sealed.chars().collect();
        let last = tampered.len() - 1;
        tampered[last] = if tampered[last] == 'A' { 'B' } else { 'A' };
        let tampered: String = tampered.into_iter().collect();
        assert!(cipher.decrypt(&tampered).is_err());
    }

    /// EDGE (empty input) and garbage input: never a panic.
    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        let cipher = Cipher::from_key([3u8; KEY_LEN]);
        for bad in ["", "!!!not base64!!!", "AAAA", &"A".repeat(15)] {
            assert!(cipher.decrypt(bad).is_err(), "{bad:?} should not decrypt");
        }
        // An empty secret is legal and round-trips.
        let sealed = cipher.encrypt("").unwrap();
        assert_eq!(cipher.decrypt(&sealed).unwrap(), "");
    }

    #[test]
    fn the_key_file_is_created_once_and_reused() {
        let path = scratch("token-key");
        let first = Cipher::from_file(&path).unwrap();
        let sealed = first.encrypt("stable").unwrap();

        let second = Cipher::from_file(&path).unwrap();
        assert_eq!(
            second.decrypt(&sealed).unwrap(),
            "stable",
            "a restart must be able to read what the last run wrote"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "the key file must not be readable");
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_env_var_wins_over_the_file() {
        let path = scratch("unused-key");
        let hex_key = hex::encode([9u8; KEY_LEN]);
        let cipher = Cipher::resolve(Some(&hex_key), &path).unwrap();
        assert!(!path.exists(), "the file must not be created when the env var is set");
        assert_eq!(
            cipher.decrypt(&Cipher::from_key([9u8; KEY_LEN]).encrypt("x").unwrap()).unwrap(),
            "x"
        );

        // A malformed env var is a loud error, never a silent fallback.
        assert!(Cipher::resolve(Some("not-hex"), &path).is_err());
        assert!(Cipher::resolve(Some("aabb"), &path).is_err());
    }
}
