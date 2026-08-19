//! Full OIDC verification for Gmail's Pub/Sub push.
//!
//! `API.md` §9 is explicit that **audience alone is forgeable and is not
//! sufficient**. Anyone can mint a token claiming our audience; only Google can
//! sign one with a key from Google's JWK Set, as a service account we named.
//! So all of the following must hold, and a failure in any of them returns a
//! byte-identical `unauthorized` envelope, so a prober cannot learn which check
//! it tripped:
//!
//! `Bearer` present → header parses → `alg` is RS256 → `kid` is one we know →
//! the signature verifies → `iss` is Google → `aud` is exactly ours → `exp`,
//! `iat` and `nbf` are inside a 60-second leeway → `email` is our push service
//! account → `email_verified` is true.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::{
    config::PushConfig,
    error::{ApiError, ApiResult},
};

/// Google signs with both of these, and both are legal.
const ISSUERS: [&str; 2] = ["https://accounts.google.com", "accounts.google.com"];

/// Tolerance on `exp`/`iat`/`nbf`. The same 60 seconds the Gmail access-token
/// refresh already allows for skew.
const LEEWAY_SECS: u64 = 60;

/// The shortest interval between two JWKS fetches provoked by an unknown `kid`.
///
/// Without it, a flood of forged tokens carrying random `kid`s would turn this
/// endpoint into a DoS amplifier pointed at Google - each forgery costing us an
/// outbound request and Google a served one.
const MIN_REFRESH: Duration = Duration::from_secs(60);

/// Clamp on the key set's cache lifetime, whatever `Cache-Control` says.
const MIN_TTL: Duration = Duration::from_secs(300);
const MAX_TTL: Duration = Duration::from_secs(24 * 3600);

/// The claims we care about. Everything else Google sends is ignored.
#[derive(Debug, Deserialize)]
pub struct Claims {
    pub iss: String,
    pub aud: String,
    pub exp: i64,
    /// Validated explicitly below: `jsonwebtoken` checks `exp` and `nbf` but has
    /// no opinion on a token *issued* in the future, and the check list needs
    /// one.
    #[serde(default)]
    pub iat: Option<i64>,
    #[serde(default)]
    pub email: Option<String>,
    /// Google sends this as a bool, and sometimes as the string `"true"`.
    /// Absent counts as **not** verified.
    #[serde(default, deserialize_with = "lenient_bool")]
    pub email_verified: bool,
}

fn lenient_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Bool(bool),
        Text(String),
    }
    Ok(match Option::<Either>::deserialize(deserializer)? {
        Some(Either::Bool(value)) => value,
        Some(Either::Text(text)) => text.eq_ignore_ascii_case("true"),
        None => false,
    })
}

#[derive(Debug, Deserialize)]
struct Jwk {
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    kty: Option<String>,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JwkSet {
    #[serde(default)]
    keys: Vec<Jwk>,
}

#[derive(Default)]
struct Cache {
    keys: HashMap<String, DecodingKey>,
    fetched_at: Option<Instant>,
    ttl: Duration,
    last_attempt: Option<Instant>,
}

impl Cache {
    /// Whether the set is inside its TTL. This decides when to **refetch** -
    /// never whether a cached key may be used.
    fn is_fresh(&self) -> bool {
        self.fetched_at.is_some_and(|at| at.elapsed() < self.ttl)
    }

    fn may_refresh(&self, gap: Duration) -> bool {
        self.last_attempt.is_none_or(|at| at.elapsed() >= gap)
    }
}

/// Verifies Pub/Sub's OIDC bearer, holding the cached key set.
pub struct Verifier {
    http: reqwest::Client,
    config: PushConfig,
    cache: RwLock<Cache>,
    /// The shortest gap between two unknown-`kid` refetches.
    ///
    /// A field rather than a constant so a test can prove both halves: that a
    /// rotated key is fetched, and that a flood of forged `kid`s is not allowed
    /// to drive one fetch each. In production it is [`MIN_REFRESH`], and the
    /// cost of the limit is that a genuine rotation can be rejected for up to
    /// that long — Pub/Sub retries, so the notification survives it.
    min_refresh: Duration,
}

impl std::fmt::Debug for Verifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Verifier")
    }
}

impl Verifier {
    /// The HTTP client must come from [`crate::gmail::http_client`]; the crate
    /// takes `rustls-no-provider`, so a client built any other way panics.
    #[must_use]
    pub fn new(http: reqwest::Client, config: PushConfig) -> Self {
        Self {
            http,
            config,
            cache: RwLock::new(Cache::default()),
            min_refresh: MIN_REFRESH,
        }
    }

    /// Tests only: shrink the refetch limit so rotation can be asserted without
    /// sleeping through a minute.
    #[cfg(test)]
    #[must_use]
    pub fn with_min_refresh(mut self, gap: Duration) -> Self {
        self.min_refresh = gap;
        self
    }

    /// Fetch the key set now, ignoring failures.
    ///
    /// Called once at boot so the first real push does not pay for it. A failed
    /// warm is a warning, never a boot failure - the server must start with no
    /// network, exactly as it starts with no Gmail client file.
    pub async fn warm(&self) {
        if let Err(error) = self.refresh().await {
            tracing::warn!(%error, "could not pre-fetch Google's JWKS; the first push will");
        }
    }

    /// Verify a push's `Authorization` header value.
    ///
    /// # Errors
    /// [`ApiError::unauthorized`] for every forgery class, with an identical
    /// body in each case. [`ApiError::upstream_unavailable`] when the key set
    /// is cold **and** unreachable: that is materially different from a forgery
    /// and must not be reported as one, because Pub/Sub retries either way but
    /// only one of them is true.
    pub async fn verify(&self, authorization: Option<&str>) -> ApiResult<Claims> {
        let token = authorization
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| self.reject("no bearer token"))?;

        let header =
            jsonwebtoken::decode_header(token).map_err(|_| self.reject("unparseable header"))?;
        if header.alg != Algorithm::RS256 {
            return Err(self.reject("algorithm is not RS256"));
        }
        let kid = header.kid.ok_or_else(|| self.reject("no key id"))?;

        // A set past its TTL is refreshed on the way past, rate-limited like any
        // other refetch. Failure is not fatal here: the keys we already hold
        // are stale, not wrong, and one of them may well be the right one.
        if !self.cache.read().await.is_fresh() {
            let _ = self.refresh_if_allowed().await;
        }

        let key = match self.key(&kid).await {
            Some(key) => key,
            None => {
                // An unknown `kid` is the one failure worth a refetch even when
                // the set is fresh: that is exactly what a rotation looks like.
                // Rate-limited, so forged `kid`s cannot drive the fetch.
                self.refresh_if_allowed().await?;
                self.key(&kid)
                    .await
                    .ok_or_else(|| self.reject("unknown key id"))?
            }
        };

        let audience =
            self.config.audience.as_deref().ok_or_else(|| {
                self.reject("no audience is configured; the webhook is fail-closed")
            })?;
        let sa_email = self
            .config
            .sa_email
            .as_deref()
            .ok_or_else(|| self.reject("no service account is configured; fail-closed"))?;

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[audience]);
        validation.set_issuer(&ISSUERS);
        validation.leeway = LEEWAY_SECS;
        validation.validate_exp = true;
        validation.validate_nbf = true;

        let data = jsonwebtoken::decode::<Claims>(token, &key, &validation)
            .map_err(|error| self.reject(&format!("signature or claims rejected: {error}")))?;

        // `aud` and `iss` are enforced above by `Validation`. These two are
        // ours, and they are the reason audience alone is not enough.
        let claims = data.claims;
        if let Some(iat) = claims.iat {
            let now = chrono::Utc::now().timestamp();
            if iat > now + i64::try_from(LEEWAY_SECS).unwrap_or(60) {
                return Err(self.reject("issued in the future"));
            }
        }
        match claims.email.as_deref() {
            // Not a constant-time compare: an address is not a secret, and
            // Google's are case-insensitive.
            Some(email) if email.eq_ignore_ascii_case(sa_email) => {}
            _ => return Err(self.reject("the email claim is not our push service account")),
        }
        if !claims.email_verified {
            return Err(self.reject("email_verified is not true"));
        }
        Ok(claims)
    }

    /// One envelope for every failure, with the reason logged rather than
    /// returned. A prober must not be able to tell "wrong audience" from
    /// "expired" from "not our service account".
    fn reject(&self, why: &str) -> ApiError {
        tracing::warn!(reason = why, "rejected a Gmail push");
        ApiError::unauthorized()
    }

    /// A cached key, **whether or not the set is still fresh**.
    ///
    /// Tying the lookup to freshness made the documented stale-key fallback
    /// unreachable: a failed refresh logged "serving a stale JWKS" and the very
    /// next lookup rejected the token anyway, because it insisted on freshness.
    /// Google rotates over days, so a key that still verifies beats dropping
    /// the user's mail.
    async fn key(&self, kid: &str) -> Option<DecodingKey> {
        self.cache.read().await.keys.get(kid).cloned()
    }

    /// Refetch because a `kid` we do not hold arrived.
    ///
    /// **Freshness must not veto this.** Google rotates, and a new `kid` signed
    /// by a genuinely new key arrives while our set is still inside its TTL - so
    /// gating on `is_fresh()` rejected every push until the TTL expired, up to
    /// the 24-hour clamp. Only the rate limit gates it, which is what stops
    /// forged tokens with random `kid`s from making this an amplifier pointed
    /// at Google.
    async fn refresh_if_allowed(&self) -> ApiResult<()> {
        if !self.cache.read().await.may_refresh(self.min_refresh) {
            // Asked too recently. The caller turns this into `unknown key id`.
            return Ok(());
        }
        match self.refresh().await {
            Ok(()) => Ok(()),
            Err(error) => {
                let cache = self.cache.read().await;
                if cache.keys.is_empty() {
                    // Cold and unreachable. Say so honestly.
                    tracing::warn!(%error, "the JWKS is cold and could not be fetched");
                    Err(ApiError::upstream_unavailable())
                } else {
                    // Warm but stale. Google rotates over days, so keys that
                    // still verify beat dropping the user's mail.
                    tracing::warn!(%error, "serving a stale JWKS");
                    Ok(())
                }
            }
        }
    }

    async fn refresh(&self) -> anyhow::Result<()> {
        {
            let mut cache = self.cache.write().await;
            cache.last_attempt = Some(Instant::now());
        }
        let response = self.http.get(&self.config.jwks_url).send().await?;
        let ttl = max_age(response.headers()).unwrap_or(self.config.jwks_ttl);
        let set: JwkSet = response.error_for_status()?.json().await?;

        let mut keys = HashMap::new();
        for jwk in set.keys {
            let (Some(kid), Some(n), Some(e)) = (jwk.kid, jwk.n, jwk.e) else {
                continue;
            };
            if jwk.kty.as_deref() != Some("RSA") {
                continue;
            }
            if let Ok(key) = DecodingKey::from_rsa_components(&n, &e) {
                keys.insert(kid, key);
            }
        }
        anyhow::ensure!(!keys.is_empty(), "the JWKS held no usable RSA keys");

        let mut cache = self.cache.write().await;
        cache.keys = keys;
        cache.fetched_at = Some(Instant::now());
        cache.ttl = ttl.clamp(MIN_TTL, MAX_TTL);
        Ok(())
    }
}

/// `Cache-Control: max-age=…`, if present.
fn max_age(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers.get(reqwest::header::CACHE_CONTROL)?.to_str().ok()?;
    for directive in value.split(',') {
        if let Some(seconds) = directive.trim().strip_prefix("max-age=") {
            return seconds.trim().parse().ok().map(Duration::from_secs);
        }
    }
    None
}
