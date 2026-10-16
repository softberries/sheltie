//! OIDC token verification against a fake key server.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{Value, json};
use sheltie_core::RepoId;
use sheltie_github::oidc::{GITHUB_ACTIONS_ISSUER, OidcConfig, OidcError, OidcVerifier};
use sheltie_github::testing::{OidcSigner, actions_claims};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn signer(kid: &str) -> OidcSigner {
    OidcSigner::new(kid)
}

fn claims() -> Value {
    actions_claims("acme/widgets", 1001)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn token(s: &OidcSigner, claims: &Value) -> String {
    s.sign(claims)
}

async fn jwks(server: &MockServer, keys: &[&OidcSigner], times: u64) {
    let keys: Vec<Value> = keys.iter().map(|k| k.jwk().clone()).collect();
    Mock::given(method("GET"))
        .and(path("/.well-known/jwks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "keys": keys })))
        .expect(times)
        .mount(server)
        .await;
}

fn verifier(server: &MockServer) -> OidcVerifier {
    OidcVerifier::new(OidcConfig {
        issuer: GITHUB_ACTIONS_ISSUER.into(),
        audience: "sheltie".into(),
        jwks_url: format!("{}/.well-known/jwks", server.uri()),
    })
    .unwrap()
}

#[tokio::test]
async fn valid_token_yields_the_repository() {
    let server = MockServer::start().await;
    let key = signer("k1");
    jwks(&server, &[&key], 1).await;
    let v = verifier(&server);
    let c = v.verify(&token(&key, &claims())).await.unwrap();
    assert_eq!(c.repository_id, RepoId(1001));
    assert_eq!(c.repository.as_str(), "acme/widgets");
    assert_eq!(c.run, "42/1");
    // A second token uses the cached keys (`expect(1)` above).
    v.verify(&token(&key, &claims())).await.unwrap();
}

#[tokio::test]
async fn wrong_audience_issuer_or_expiry_is_rejected() {
    let server = MockServer::start().await;
    let key = signer("k1");
    jwks(&server, &[&key], 1).await;
    let v = verifier(&server);
    for (field, value) in [
        ("aud", json!("someone-else")),
        ("iss", json!("https://evil.example")),
        ("exp", json!(now() - 3600)),
        ("nbf", json!(now() + 3600)),
    ] {
        let mut c = claims();
        c[field] = value;
        let err = v.verify(&token(&key, &c)).await.unwrap_err();
        assert!(matches!(err, OidcError::Rejected(_)), "{field}: {err}");
    }
}

#[tokio::test]
async fn forged_signature_is_rejected() {
    let server = MockServer::start().await;
    let published = signer("k1");
    jwks(&server, &[&published], 1).await;
    // Same key id, different private key.
    let forger = signer("k1");
    let err = verifier(&server)
        .verify(&token(&forger, &claims()))
        .await
        .unwrap_err();
    assert!(matches!(err, OidcError::Rejected(_)), "{err}");
}

#[tokio::test]
async fn other_algorithms_and_missing_key_ids_are_malformed() {
    let server = MockServer::start().await;
    let v = verifier(&server);
    let hs = jsonwebtoken::encode(
        &Header::new(Algorithm::HS256),
        &claims(),
        &EncodingKey::from_secret(b"x"),
    )
    .unwrap();
    assert!(matches!(v.verify(&hs).await, Err(OidcError::Malformed)));
    let rsa = EncodingKey::from_rsa_pem(&sheltie_github::testing::rsa_pem()).unwrap();
    let no_kid = jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims(), &rsa).unwrap();
    assert!(matches!(v.verify(&no_kid).await, Err(OidcError::Malformed)));
    assert!(matches!(
        v.verify("not.a.jwt").await,
        Err(OidcError::Malformed)
    ));
}

#[tokio::test]
async fn unknown_key_does_not_refetch_more_than_once_a_minute() {
    let server = MockServer::start().await;
    let old = signer("old");
    jwks(&server, &[&old], 1).await;
    let v = verifier(&server);
    v.verify(&token(&old, &claims())).await.unwrap();
    let rotated = signer("new");
    for _ in 0..3 {
        assert!(matches!(
            v.verify(&token(&rotated, &claims())).await,
            Err(OidcError::UnknownKey)
        ));
    }
    // `expect(1)`: the key set was fetched once despite four verifications.
}

#[tokio::test]
async fn unreachable_key_server_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let key = signer("k1");
    assert!(matches!(
        verifier(&server).verify(&token(&key, &claims())).await,
        Err(OidcError::Jwks(_))
    ));
}
