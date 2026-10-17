//! Test helpers: throwaway RSA keys, signed OIDC tokens and a fake key server.
//! Only built with the `test-support` feature.

use aws_lc_rs::encoding::{AsDer, Pkcs8V1Der};
use aws_lc_rs::rsa::{KeyPair, KeySize};
use aws_lc_rs::signature::KeyPair as _;
use base64::Engine as _;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{Value, json};

use crate::oidc::GITHUB_ACTIONS_ISSUER;

/// A fresh 2048-bit RSA key in PKCS#8 PEM. Generated per test, so no key is committed.
///
/// # Panics
/// If key generation fails, which only happens on a broken crypto backend.
pub fn rsa_pem() -> Vec<u8> {
    pem(&generate())
}

fn generate() -> KeyPair {
    KeyPair::generate(KeySize::Rsa2048).unwrap_or_else(|e| panic!("RSA key generation: {e}"))
}

fn pem(key: &KeyPair) -> Vec<u8> {
    let der = AsDer::<Pkcs8V1Der>::as_der(key).unwrap_or_else(|e| panic!("PKCS#8 export: {e}"));
    let b64 = base64::engine::general_purpose::STANDARD.encode(der.as_ref());
    let lines: Vec<String> = b64
        .as_bytes()
        .chunks(64)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect();
    format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        lines.join("\n")
    )
    .into_bytes()
}

/// Signs GitHub-Actions-style OIDC tokens and publishes the matching JWK.
pub struct OidcSigner {
    kid: String,
    encoding: EncodingKey,
    jwk: Value,
}

impl OidcSigner {
    /// A new signer with key id `kid`.
    ///
    /// # Panics
    /// If key generation fails.
    pub fn new(kid: &str) -> Self {
        let key = generate();
        let url = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let public = key.public_key();
        let jwk = json!({
            "kty": "RSA", "alg": "RS256", "use": "sig", "kid": kid,
            "n": url.encode(public.modulus().big_endian_without_leading_zero()),
            "e": url.encode(public.exponent().big_endian_without_leading_zero()),
        });
        let encoding =
            EncodingKey::from_rsa_pem(&pem(&key)).unwrap_or_else(|e| panic!("encoding key: {e}"));
        Self {
            kid: kid.to_owned(),
            encoding,
            jwk,
        }
    }

    /// The public key as a JWK.
    pub fn jwk(&self) -> &Value {
        &self.jwk
    }

    /// A JWKS document containing this key.
    pub fn jwks(&self) -> Value {
        json!({ "keys": [self.jwk] })
    }

    /// Signs `claims` with key id `kid` (normally this signer's own).
    ///
    /// # Panics
    /// If signing fails.
    pub fn sign_as(&self, kid: &str, claims: &Value) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(kid.to_owned());
        jsonwebtoken::encode(&header, claims, &self.encoding)
            .unwrap_or_else(|e| panic!("signing: {e}"))
    }

    /// Signs `claims` with this signer's key id.
    pub fn sign(&self, claims: &Value) -> String {
        self.sign_as(&self.kid, claims)
    }
}

/// Claims of a valid token for `repository` (`owner/name`, id `repository_id`) with
/// audience `sheltie`, valid for five minutes.
pub fn actions_claims(repository: &str, repository_id: u64) -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    json!({
        "iss": GITHUB_ACTIONS_ISSUER,
        "aud": "sheltie",
        "sub": format!("repo:{repository}:pull_request"),
        "iat": now - 10,
        "nbf": now - 10,
        "exp": now + 300,
        "repository": repository,
        "repository_id": repository_id.to_string(),
        "event_name": "pull_request",
        "ref": "refs/pull/1/merge",
        "run_id": "42",
        "run_attempt": "1",
    })
}

/// Claims of a GitHub Actions token for a push to `branch`.
pub fn push_claims(repository: &str, repository_id: u64, branch: &str) -> Value {
    let mut claims = actions_claims(repository, repository_id);
    claims["sub"] = json!(format!("repo:{repository}:ref:refs/heads/{branch}"));
    claims["event_name"] = json!("push");
    claims["ref"] = json!(format!("refs/heads/{branch}"));
    claims
}
