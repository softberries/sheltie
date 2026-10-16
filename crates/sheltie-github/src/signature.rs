//! Webhook signature verification (`X-Hub-Signature-256`).
//!
//! Security-sensitive: every webhook body must pass [`WebhookSecret::verify`] before any byte
//! of it is parsed.

use std::fmt;

use hmac::{Hmac, KeyInit, Mac};
use secrecy::{ExposeSecret, SecretString};
use sha2::Sha256;

/// Why a webhook signature was rejected. Deliberately coarse: callers answer 401 either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SignatureError {
    /// The `X-Hub-Signature-256` header is absent.
    #[error("missing signature header")]
    Missing,
    /// The header is not `sha256=<64 hex digits>`.
    #[error("malformed signature header")]
    Malformed,
    /// The signature does not match the body.
    #[error("signature mismatch")]
    Mismatch,
}

/// The App's webhook secret. Never printed: `Debug` is redacted.
#[derive(Clone)]
pub struct WebhookSecret(SecretString);

impl WebhookSecret {
    /// Wraps the secret configured for the GitHub App. Returns `None` for an empty secret,
    /// which would make every signature trivially forgeable.
    pub fn new(secret: impl Into<String>) -> Option<Self> {
        let secret: String = secret.into();
        (!secret.is_empty()).then(|| Self(SecretString::from(secret)))
    }

    /// Checks `header` (the `X-Hub-Signature-256` value) against `body`, in constant time.
    pub fn verify(&self, body: &[u8], header: Option<&str>) -> Result<(), SignatureError> {
        let header = header.ok_or(SignatureError::Missing)?;
        let hex_sig = header
            .strip_prefix("sha256=")
            .ok_or(SignatureError::Malformed)?;
        let mut expected = [0u8; 32];
        hex::decode_to_slice(hex_sig, &mut expected).map_err(|_| SignatureError::Malformed)?;
        self.mac(body)
            .verify_slice(&expected)
            .map_err(|_| SignatureError::Mismatch)
    }

    /// Computes the header value GitHub would send for `body`. Used by tests and tooling.
    pub fn sign(&self, body: &[u8]) -> String {
        format!(
            "sha256={}",
            hex::encode(self.mac(body).finalize().into_bytes())
        )
    }

    fn mac(&self, body: &[u8]) -> Hmac<Sha256> {
        // HMAC accepts keys of any length, so this cannot fail.
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(self.0.expose_secret().as_bytes())
            .unwrap_or_else(|_| unreachable!("HMAC takes keys of any length"));
        mac.update(body);
        mac
    }
}

impl fmt::Debug for WebhookSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WebhookSecret(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn secret() -> WebhookSecret {
        WebhookSecret::new("It's a Secret to Everybody").unwrap()
    }

    /// The worked example from GitHub's "Validating webhook deliveries" documentation.
    #[test]
    fn matches_githubs_documented_example() {
        let header = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
        assert_eq!(secret().sign(b"Hello, World!"), header);
        assert_eq!(secret().verify(b"Hello, World!", Some(header)), Ok(()));
    }

    #[test]
    fn rejects_missing_and_malformed_headers() {
        let s = secret();
        assert_eq!(s.verify(b"x", None), Err(SignatureError::Missing));
        for bad in [
            "",
            "sha1=abc",
            "sha256=",
            "sha256=zz",
            &format!("sha256={}", "a".repeat(63)),
        ] {
            assert_eq!(
                s.verify(b"x", Some(bad)),
                Err(SignatureError::Malformed),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn rejects_other_secret() {
        let other = WebhookSecret::new("other").unwrap();
        let header = other.sign(b"body");
        assert_eq!(
            secret().verify(b"body", Some(&header)),
            Err(SignatureError::Mismatch)
        );
    }

    #[test]
    fn empty_secret_is_refused() {
        assert!(WebhookSecret::new("").is_none());
    }

    #[test]
    fn debug_is_redacted() {
        assert!(!format!("{:?}", secret()).contains("Secret to"));
    }

    proptest! {
        #[test]
        fn any_body_change_is_rejected(
            body in prop::collection::vec(any::<u8>(), 0..256),
            flip in any::<prop::sample::Index>(),
            bit in 0u8..8,
        ) {
            let s = secret();
            let header = s.sign(&body);
            prop_assert_eq!(s.verify(&body, Some(&header)), Ok(()));
            if !body.is_empty() {
                let mut tampered = body.clone();
                tampered[flip.index(body.len())] ^= 1 << bit;
                prop_assert_eq!(s.verify(&tampered, Some(&header)), Err(SignatureError::Mismatch));
            }
        }

        #[test]
        fn any_signature_change_is_rejected(
            body in prop::collection::vec(any::<u8>(), 0..64),
            flip in 0usize..32,
            bit in 0u8..8,
        ) {
            let s = secret();
            let header = s.sign(&body);
            let mut sig = hex::decode(&header["sha256=".len()..]).unwrap();
            sig[flip] ^= 1 << bit;
            let tampered = format!("sha256={}", hex::encode(sig));
            prop_assert_eq!(s.verify(&body, Some(&tampered)), Err(SignatureError::Mismatch));
        }
    }
}
