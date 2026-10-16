//! The GitHub client and check-run sync against a fake GitHub and a throwaway database.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::sync::Arc;

use serde_json::json;
use sheltie_core::{DeliveryId, InstallationId, RepoFullName};
use sheltie_github::check_run::{CheckConclusion, CheckRunSpec, CheckStatus};
use sheltie_github::{CheckRunSync, GitHubApp, payload};
use sheltie_store::Store;
use sqlx::PgPool;
use support::{INSTALLATION, app_key_pem, check_run_body, mock_token};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn app(server: &MockServer) -> GitHubApp {
    GitHubApp::new(123456, &app_key_pem(), Some(&server.uri())).unwrap()
}

fn spec(status: CheckStatus) -> CheckRunSpec {
    CheckRunSpec {
        status,
        title: "Title".into(),
        summary: "Summary".into(),
    }
}

#[test]
fn rejects_a_key_that_is_not_rsa_pem() {
    let err = GitHubApp::new(
        1,
        b"-----BEGIN PRIVATE KEY-----\nnope\n-----END PRIVATE KEY-----\n",
        None,
    )
    .unwrap_err();
    assert!(matches!(err, sheltie_github::GitHubError::InvalidKey));
}

#[tokio::test]
async fn creates_and_updates_check_runs_reusing_the_installation_token() {
    let server = MockServer::start().await;
    mock_token(&server, 1).await;
    let head = "1".repeat(40);
    Mock::given(method("POST"))
        .and(path("/repos/acme/widgets/check-runs"))
        .and(header(
            "authorization",
            "Bearer ghs_test_installation_token",
        ))
        .and(body_partial_json(json!({
            "name": "Sheltie",
            "head_sha": head,
            "external_id": "17",
            "status": "queued",
            "output": { "title": "Title", "summary": "Summary" }
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(check_run_body(31, &head)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/acme/widgets/check-runs/31"))
        .and(body_partial_json(
            json!({ "status": "completed", "conclusion": "neutral" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(check_run_body(31, &head)))
        .expect(1)
        .mount(&server)
        .await;

    let github = app(&server);
    let repo = RepoFullName::parse("acme/widgets").unwrap();
    let installation = InstallationId(INSTALLATION);
    let id = github
        .create_check_run(
            installation,
            &repo,
            &head.parse().unwrap(),
            "17",
            &spec(CheckStatus::Queued),
        )
        .await
        .unwrap();
    assert_eq!(id, 31);
    github
        .update_check_run(
            installation,
            &repo,
            id,
            &spec(CheckStatus::Completed(CheckConclusion::Neutral)),
        )
        .await
        .unwrap();
    // `expect(1)` on the token mock verifies the token was cached across both calls.
}

async fn ingest_fixture(store: &Store, delivery: &str, event: &str, file: &str) {
    let body = std::fs::read(format!(
        "{}/tests/fixtures/{file}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let intake = payload::parse(event, &body).unwrap().unwrap();
    store
        .ingest(&DeliveryId::new(delivery), event, &intake)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn sync_posts_new_heads_and_completes_superseded_ones(pool: PgPool) {
    let server = MockServer::start().await;
    mock_token(&server, 1).await;
    let (old, new) = ("1".repeat(40), "2".repeat(40));
    Mock::given(method("POST"))
        .and(path("/repos/acme/widgets/check-runs"))
        .and(body_partial_json(
            json!({ "head_sha": old, "status": "queued" }),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(check_run_body(31, &old)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/widgets/check-runs"))
        .and(body_partial_json(
            json!({ "head_sha": new, "status": "queued" }),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(check_run_body(32, &new)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/acme/widgets/check-runs/31"))
        .and(body_partial_json(
            json!({ "status": "completed", "conclusion": "neutral" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(check_run_body(31, &old)))
        .expect(1)
        .mount(&server)
        .await;

    let store = Store::from_pool(pool);
    let sync = CheckRunSync::new(store.clone(), Arc::new(app(&server)));

    ingest_fixture(&store, "d1", "pull_request", "pull_request.opened.json").await;
    assert_eq!(sync.run_once().await.unwrap(), 1);
    ingest_fixture(
        &store,
        "d2",
        "pull_request",
        "pull_request.synchronize.json",
    )
    .await;
    assert_eq!(sync.run_once().await.unwrap(), 2);
    assert_eq!(sync.run_once().await.unwrap(), 0, "everything is synced");
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn failed_sync_is_recorded_and_retried_later(pool: PgPool) {
    let server = MockServer::start().await;
    mock_token(&server, 1).await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/widgets/check-runs"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({ "message": "boom" })))
        .mount(&server)
        .await;

    let store = Store::from_pool(pool.clone());
    let sync = CheckRunSync::new(store.clone(), Arc::new(app(&server)));
    ingest_fixture(&store, "d1", "pull_request", "pull_request.opened.json").await;
    assert_eq!(sync.run_once().await.unwrap(), 1);
    assert_eq!(sync.run_once().await.unwrap(), 0, "backing off");

    let (attempts, error): (i32, Option<String>) =
        sqlx::query_as("SELECT attempts, last_error FROM check_runs")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(attempts, 1);
    assert!(error.unwrap().contains("GitHub API request failed"));
}
