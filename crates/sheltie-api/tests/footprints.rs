//! `POST /v1/footprints` against a throwaway database and a fake OIDC key server.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sheltie_api::{Footprints, router};
use sheltie_core::intake::{InstallationEvent, Intake, RepoRef};
use sheltie_core::{DeliveryId, InstallationId, Login, RepoFullName, RepoId};
use sheltie_github::oidc::{GITHUB_ACTIONS_ISSUER, OidcConfig, OidcVerifier};
use sheltie_github::testing::{OidcSigner, actions_claims};
use sheltie_store::Store;
use sqlx::PgPool;
use tokio::sync::Notify;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Harness {
    store: Store,
    footprints: Footprints,
    signer: OidcSigner,
    _jwks: MockServer,
}

async fn harness(pool: PgPool) -> Harness {
    let jwks = MockServer::start().await;
    let signer = OidcSigner::new("k1");
    Mock::given(method("GET"))
        .and(path("/.well-known/jwks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(signer.jwks()))
        .mount(&jwks)
        .await;
    let verifier = OidcVerifier::new(OidcConfig {
        issuer: GITHUB_ACTIONS_ISSUER.into(),
        audience: "sheltie".into(),
        jwks_url: format!("{}/.well-known/jwks", jwks.uri()),
    })
    .unwrap();
    let store = Store::from_pool(pool);
    let install = Intake::Installation(InstallationEvent::Created {
        installation: InstallationId(77),
        account: Login::new("acme"),
        repos: vec![RepoRef {
            id: RepoId(1001),
            full_name: RepoFullName::parse("acme/widgets").unwrap(),
        }],
    });
    store
        .ingest(&DeliveryId::new("install"), "installation", &install)
        .await
        .unwrap();
    Harness {
        store,
        footprints: Footprints {
            verifier: Arc::new(verifier),
            wake: Arc::new(Notify::new()),
        },
        signer,
        _jwks: jwks,
    }
}

fn report(writes: &[&str], reads: &[&str]) -> Value {
    json!({
        "adapter": "sheltie-cargo/test",
        "base": "0".repeat(40),
        "head": "1".repeat(40),
        "base_tree": "e".repeat(40),
        "diff_hash": "sha256:abc",
        "footprint": { "writes": writes, "reads": reads, "tests": [], "leaky": false },
        "leak_reasons": []
    })
}

impl Harness {
    async fn upload(&self, token: Option<&str>, body: &[u8]) -> (StatusCode, Value) {
        let mut request =
            Request::post("/v1/footprints").header("content-type", "application/json");
        if let Some(t) = token {
            request = request.header("authorization", format!("Bearer {t}"));
        }
        let response = router(
            self.store.clone(),
            None,
            Some(self.footprints.clone()),
            None,
            None,
        )
        .oneshot(request.body(Body::from(body.to_vec())).unwrap())
        .await
        .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    fn token(&self, repo: &str, id: u64) -> String {
        self.signer.sign(&actions_claims(repo, id))
    }
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn upload_with_a_valid_token_is_stored(pool: PgPool) {
    let h = harness(pool.clone()).await;
    let body = serde_json::to_vec(&report(&["a"], &["a", "b"])).unwrap();
    let (status, reply) = h.upload(Some(&h.token("acme/widgets", 1001)), &body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{reply}");
    assert_eq!(reply, json!({ "status": "accepted", "changes": 0 }));
    let uploaded_by: String = sqlx::query_scalar("SELECT uploaded_by FROM footprint_reports")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(uploaded_by, "acme/widgets pull_request run 42/1");
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn missing_or_forged_tokens_are_unauthorized(pool: PgPool) {
    let h = harness(pool).await;
    let body = serde_json::to_vec(&report(&["a"], &["a"])).unwrap();
    assert_eq!(h.upload(None, &body).await.0, StatusCode::UNAUTHORIZED);
    let forger = OidcSigner::new("k1");
    let forged = forger.sign(&actions_claims("acme/widgets", 1001));
    assert_eq!(
        h.upload(Some(&forged), &body).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn repositories_without_the_app_are_forbidden(pool: PgPool) {
    let h = harness(pool).await;
    let body = serde_json::to_vec(&report(&["a"], &["a"])).unwrap();
    let (status, reply) = h.upload(Some(&h.token("evil/repo", 666)), &body).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(reply["status"], "repository_not_installed");
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn malformed_and_invalid_reports_are_rejected(pool: PgPool) {
    let h = harness(pool).await;
    let token = h.token("acme/widgets", 1001);
    assert_eq!(
        h.upload(Some(&token), b"{").await.0,
        StatusCode::BAD_REQUEST
    );
    let mut bad_sha = report(&["a"], &["a"]);
    bad_sha["head"] = json!("not-a-sha");
    let body = serde_json::to_vec(&bad_sha).unwrap();
    assert_eq!(
        h.upload(Some(&token), &body).await.0,
        StatusCode::BAD_REQUEST
    );
    let body = serde_json::to_vec(&report(&["a"], &["b"])).unwrap();
    let (status, reply) = h.upload(Some(&token), &body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(reply["status"], "invalid_report");
}
