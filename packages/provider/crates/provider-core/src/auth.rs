//! Session-token verification.
//!
//! The coordinator signs Ed25519 JWTs; the provider verifies them against a
//! JWKS it fetches once at startup and caches. Consequences that are the whole
//! point of this design:
//!
//! * no shared secret is ever distributed to a provider;
//! * a provider keeps serving an already-authorized session while the
//!   coordinator is restarting, or gone entirely;
//! * revocation is a control-plane **push**, not a token-lifetime side effect —
//!   the short `exp` is only a backstop.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use base64::Engine as _;
use yard_protocol::SESSION_TOKEN_AUDIENCE;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// A JWKS refetch is rate-limited to this, so an attacker cannot make the
/// provider hammer the coordinator by presenting tokens with unknown `kid`s.
const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// Tolerance for clock skew between the coordinator and this host.
///
/// Deliberately small. Session tokens live ~60s, so the library's 60s default
/// would let an expired one keep working for another full lifetime.
const CLOCK_SKEW_LEEWAY_SECS: u64 = 5;

#[derive(Debug, Clone, Deserialize)]
pub struct SessionClaims {
    pub iss: String,
    pub sub: String,
    pub exp: i64,
    pub iat: i64,
    #[serde(rename = "deviceId")]
    pub device_id: String,
    #[serde(rename = "userId")]
    pub user_id: String,
    #[serde(rename = "reservationId")]
    pub reservation_id: String,
    #[serde(rename = "providerId")]
    pub provider_id: String,
}

#[derive(Debug, Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Debug, Deserialize)]
struct Jwk {
    kty: String,
    #[serde(default)]
    crv: String,
    /// base64url raw Ed25519 public key.
    x: String,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    alg: Option<String>,
}

struct CachedKey {
    kid: Option<String>,
    key: DecodingKey,
}

pub struct TokenVerifier {
    jwks_url: String,
    provider_id: String,
    /// Set from `hello.ack`, because only the coordinator knows it.
    ///
    /// A provider dials an address; tokens are signed with the origin browsers
    /// use. Those differ in every deployment that is not a laptop, so this
    /// starts as the configured address and is corrected on registration.
    issuer: std::sync::RwLock<String>,
    http: reqwest::Client,
    keys: RwLock<Vec<Arc<CachedKey>>>,
    last_refresh: RwLock<Option<std::time::Instant>>,
}

impl TokenVerifier {
    pub fn new(jwks_url: String, provider_id: String, issuer: String) -> Self {
        Self {
            jwks_url,
            provider_id,
            issuer: std::sync::RwLock::new(issuer),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("building the JWKS http client"),
            keys: RwLock::new(Vec::new()),
            last_refresh: RwLock::new(None),
        }
    }

    /// Adopt the issuer the coordinator reported in `hello.ack`.
    ///
    /// Tokens minted before this lands are rejected, which is correct: until
    /// the provider has registered it has no authority to trust anything.
    pub fn set_issuer(&self, issuer: String) {
        if let Ok(mut current) = self.issuer.write() {
            if *current != issuer {
                info!(from = %current, to = %issuer, "issuer updated from hello.ack");
                *current = issuer;
            }
        }
    }

    /// Fetches the JWKS. Called at startup and, at most once a minute, when a
    /// token arrives with a `kid` we do not know — which is what happens after
    /// the coordinator rotates its key.
    pub async fn refresh(&self) -> Result<usize> {
        let response = self
            .http
            .get(&self.jwks_url)
            .send()
            .await
            .with_context(|| format!("fetching {}", self.jwks_url))?
            .error_for_status()
            .context("JWKS endpoint returned an error status")?;

        let jwks: Jwks = response.json().await.context("parsing JWKS")?;

        let mut parsed = Vec::new();
        for jwk in jwks.keys {
            // Only Ed25519. Accepting anything else would open the door to
            // algorithm confusion, which is the classic JWT verification bug.
            if jwk.kty != "OKP" || jwk.crv != "Ed25519" {
                warn!(kty = %jwk.kty, crv = %jwk.crv, "ignoring non-Ed25519 JWKS entry");
                continue;
            }
            if let Some(alg) = &jwk.alg {
                if alg != "EdDSA" {
                    warn!(%alg, "ignoring JWKS entry with unexpected alg");
                    continue;
                }
            }

            let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(jwk.x.as_bytes())
                .context("JWKS key `x` is not valid base64url")?;

            parsed.push(Arc::new(CachedKey {
                kid: jwk.kid,
                key: DecodingKey::from_ed_der(&raw),
            }));
        }

        if parsed.is_empty() {
            bail!("JWKS contained no usable Ed25519 keys");
        }

        let count = parsed.len();
        *self.keys.write().await = parsed;
        *self.last_refresh.write().await = Some(std::time::Instant::now());
        info!(count, url = %self.jwks_url, "loaded coordinator JWKS");
        Ok(count)
    }

    /// Exercises the verification path once at startup.
    ///
    /// Verification only runs when a user opens a session, so a broken crypto
    /// backend would otherwise surface as a failed session hours after deploy —
    /// which is exactly what happened during development, when `jsonwebtoken`
    /// panicked on its missing crypto-provider feature at first use. This makes
    /// that class of failure happen at boot, loudly, before anyone is affected.
    pub async fn self_test(&self) -> Result<()> {
        // Well-formed, signed by nobody. Must be *rejected*, not blow up.
        const NOT_OURS: &str =
            "eyJhbGciOiJFZERTQSIsInR5cCI6IkpXVCJ9.eyJzdWIiOiJ0ZXN0In0.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

        match self.verify(NOT_OURS).await {
            Ok(_) => bail!("self-test: a token we never signed verified successfully"),
            Err(_) => Ok(()),
        }
    }

    /// Verifies a session token and returns its claims.
    ///
    /// Checks, in order: signature (EdDSA only), issuer, audience, expiry, and
    /// that the token was minted for *this* provider — a token for another
    /// provider is a valid signature over the wrong subject, and must not pass.
    pub async fn verify(&self, token: &str) -> Result<SessionClaims> {
        let kid = decode_kid(token);

        let mut validation = Validation::new(Algorithm::EdDSA);
        let issuer = self
            .issuer
            .read()
            .map(|issuer| issuer.clone())
            .unwrap_or_default();
        validation.set_issuer(&[issuer.as_str()]);
        validation.set_audience(&[SESSION_TOKEN_AUDIENCE]);
        validation.set_required_spec_claims(&["exp", "aud", "iss"]);
        // jsonwebtoken defaults to 60s of leeway, which would *double* the
        // lifetime of a ~60s session token. Allow only enough for ordinary
        // clock skew between the coordinator and this host.
        validation.leeway = CLOCK_SKEW_LEEWAY_SECS;

        if let Some(claims) = self.try_verify(token, kid.as_deref(), &validation).await? {
            return self.check_provider(claims);
        }

        // Unknown `kid`: the coordinator may have rotated. Refetch at most once
        // a minute so an attacker cannot turn bad tokens into a request flood.
        if self.should_refresh().await {
            self.refresh().await?;
            if let Some(claims) = self.try_verify(token, kid.as_deref(), &validation).await? {
                return self.check_provider(claims);
            }
        }

        bail!("no JWKS key verified this token")
    }

    async fn try_verify(
        &self,
        token: &str,
        kid: Option<&str>,
        validation: &Validation,
    ) -> Result<Option<SessionClaims>> {
        let keys = self.keys.read().await.clone();
        if keys.is_empty() {
            bail!("JWKS has not been loaded yet");
        }

        let mut last_error = None;
        for entry in keys {
            // When both sides name a kid, only that key is a candidate.
            if let (Some(want), Some(have)) = (kid, entry.kid.as_deref()) {
                if want != have {
                    continue;
                }
            }
            match jsonwebtoken::decode::<SessionClaims>(token, &entry.key, validation) {
                Ok(data) => return Ok(Some(data.claims)),
                Err(err) => last_error = Some(err),
            }
        }

        // A signature that matched but failed a claim check is a real rejection
        // and must not be retried as a rotation.
        if let Some(err) = last_error {
            use jsonwebtoken::errors::ErrorKind;
            if !matches!(err.kind(), ErrorKind::InvalidSignature) {
                return Err(anyhow::Error::new(err).context("session token rejected"));
            }
        }
        Ok(None)
    }

    fn check_provider(&self, claims: SessionClaims) -> Result<SessionClaims> {
        if claims.provider_id != self.provider_id {
            bail!(
                "token was minted for provider {:?}, not {:?}",
                claims.provider_id,
                self.provider_id
            );
        }
        Ok(claims)
    }

    async fn should_refresh(&self) -> bool {
        match *self.last_refresh.read().await {
            Some(at) => at.elapsed() >= MIN_REFRESH_INTERVAL,
            None => true,
        }
    }
}

/// Reads `kid` out of the JOSE header without verifying anything.
///
/// Header contents are untrusted until the signature checks out; this is only
/// used to pick which candidate key to *try*.
fn decode_kid(token: &str) -> Option<String> {
    let header = jsonwebtoken::decode_header(token).ok()?;
    header.kid
}
