//! Verifying GitHub Actions OIDC tokens, which authenticate footprint uploads.
//!
//! Security-sensitive. A token proves which repository's workflow run is calling; it is
//! checked against GitHub's published signing keys (RS256 only), the configured issuer and
//! audience, and its validity window. No secrets are shared with the CI runner.

use std::time::{Duration, Instant};

use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use sheltie_core::{RepoFullName, RepoId};
use tokio::sync::RwLock;

/// The issuer of GitHub Actions tokens on github.com.
pub const GITHUB_ACTIONS_ISSUER: &str = "https://token.actions.githubusercontent.com";

/// Audience Sheltie expects unless configured otherwise.
pub const DEFAULT_AUDIENCE: &str = "sheltie";

/// Signing keys are refetched at least this often.
const JWKS_MAX_AGE: Duration = Duration::from_secs(3600);
/// An unknown key id triggers a refetch, but not more often than this.
const JWKS_MIN_REFRESH: Duration = Duration::from_secs(60);
/// Clock skew tolerated on `exp`, `nbf` and `iat`.
const LEEWAY_SECS: u64 = 60;

/// Where and what to verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcConfig {
    /// Expected `iss`.
    pub issuer: String,
    /// Expected `aud`.
    pub audience: String,
    /// Where the issuer publishes its signing keys.
    pub jwks_url: String,
}

impl OidcConfig {
    /// GitHub Actions on github.com with the given audience.
    pub fn github_actions(audience: impl Into<String>) -> Self {
        Self {
            issuer: GITHUB_ACTIONS_ISSUER.to_owned(),
            audience: audience.into(),
            jwks_url: format!("{GITHUB_ACTIONS_ISSUER}/.well-known/jwks"),
        }
    }
}

/// Why a token was rejected. Callers answer 401 for all of them.
#[derive(Debug, thiserror::Error)]
pub enum OidcError {
    /// The token is not a well-formed RS256 JWT with a key id.
    #[error("malformed token")]
    Malformed,
    /// No published key matches the token's key id.
    #[error("unknown signing key")]
    UnknownKey,
    /// The signing keys could not be fetched.
    #[error("cannot fetch signing keys: {0}")]
    Jwks(String),
    /// Signature, issuer, audience or validity window check failed.
    #[error("token rejected: {0}")]
    Rejected(String),
}

/// The claims Sheltie uses from a verified token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionsClaims {
    /// The repository whose workflow requested the token.
    pub repository_id: RepoId,
    /// Its `owner/name`.
    pub repository: RepoFullName,
    /// Event that triggered the run, e.g. `pull_request`.
    pub event_name: String,
    /// The workflow run, for audit (`<run_id>/<attempt>`).
    pub run: String,
}

#[derive(Deserialize)]
struct RawClaims {
    repository_id: String,
    repository: String,
    #[serde(default)]
    event_name: String,
    #[serde(default)]
    run_id: String,
    #[serde(default)]
    run_attempt: String,
}

struct Keys {
    set: JwkSet,
    fetched_at: Instant,
}

/// Verifies tokens, caching the issuer's signing keys.
pub struct OidcVerifier {
    config: OidcConfig,
    http: reqwest::Client,
    keys: RwLock<Option<Keys>>,
}

impl OidcVerifier {
    /// Creates a verifier; keys are fetched on first use.
    pub fn new(config: OidcConfig) -> Result<Self, OidcError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| OidcError::Jwks(e.to_string()))?;
        Ok(Self {
            config,
            http,
            keys: RwLock::new(None),
        })
    }

    /// Verifies `token` and returns its claims.
    pub async fn verify(&self, token: &str) -> Result<ActionsClaims, OidcError> {
        let header = jsonwebtoken::decode_header(token).map_err(|_| OidcError::Malformed)?;
        if header.alg != Algorithm::RS256 {
            return Err(OidcError::Malformed);
        }
        let kid = header.kid.ok_or(OidcError::Malformed)?;
        let key = self.key(&kid).await?;

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[&self.config.issuer]);
        validation.set_audience(&[&self.config.audience]);
        validation.set_required_spec_claims(&["exp", "iat", "iss", "aud"]);
        validation.validate_nbf = true;
        validation.leeway = LEEWAY_SECS;
        let data = jsonwebtoken::decode::<RawClaims>(token, &key, &validation)
            .map_err(|e| OidcError::Rejected(e.kind_description()))?;
        let c = data.claims;
        Ok(ActionsClaims {
            repository_id: RepoId(
                c.repository_id
                    .parse()
                    .map_err(|_| OidcError::Rejected("repository_id is not a number".into()))?,
            ),
            repository: RepoFullName::parse(c.repository)
                .map_err(|e| OidcError::Rejected(e.to_string()))?,
            event_name: c.event_name,
            run: format!("{}/{}", c.run_id, c.run_attempt),
        })
    }

    /// The decoding key for `kid`, refetching the key set when it is stale or lacks `kid`.
    async fn key(&self, kid: &str) -> Result<DecodingKey, OidcError> {
        {
            let keys = self.keys.read().await;
            if let Some(k) = keys.as_ref()
                && k.fetched_at.elapsed() < JWKS_MAX_AGE
            {
                if let Some(jwk) = k.set.find(kid) {
                    return DecodingKey::from_jwk(jwk).map_err(|_| OidcError::UnknownKey);
                }
                if k.fetched_at.elapsed() < JWKS_MIN_REFRESH {
                    return Err(OidcError::UnknownKey);
                }
            }
        }
        let mut keys = self.keys.write().await;
        // Another request may have refreshed while we waited for the lock.
        let fresh = keys
            .as_ref()
            .is_some_and(|k| k.fetched_at.elapsed() < JWKS_MIN_REFRESH);
        if !fresh {
            let set: JwkSet = self
                .http
                .get(&self.config.jwks_url)
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .map_err(|e| OidcError::Jwks(e.to_string()))?
                .json()
                .await
                .map_err(|e| OidcError::Jwks(e.to_string()))?;
            *keys = Some(Keys {
                set,
                fetched_at: Instant::now(),
            });
        }
        let jwk = keys
            .as_ref()
            .and_then(|k| k.set.find(kid))
            .ok_or(OidcError::UnknownKey)?;
        DecodingKey::from_jwk(jwk).map_err(|_| OidcError::UnknownKey)
    }
}

impl std::fmt::Debug for OidcVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OidcVerifier")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

trait KindDescription {
    fn kind_description(&self) -> String;
}

impl KindDescription for jsonwebtoken::errors::Error {
    /// The error kind without token contents.
    fn kind_description(&self) -> String {
        format!("{:?}", self.kind())
    }
}
