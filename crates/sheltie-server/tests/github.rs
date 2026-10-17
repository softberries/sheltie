//! End to end: a pull request webhook leads to a "Sheltie" check run on its head (M1), and a
//! footprint uploaded from CI with an OIDC token shows up on that check run (M2).
//!
//! Runs the real server against a throwaway database and a fake GitHub (wiremock), which also
//! serves the OIDC signing keys.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use serde_json::json;
use sheltie_github::WebhookSecret;
use sheltie_github::oidc::{GITHUB_ACTIONS_ISSUER, OidcConfig};
use sheltie_github::testing::{OidcSigner, actions_claims};
use sheltie_server::{Config, GitHubConfig, Server};
use sheltie_store::PoolConfig;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::sync::oneshot;
use wiremock::matchers::{body_partial_json, body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SECRET: &str = "e2e-webhook-secret";

/// Writes a fresh App key under the test's scratch directory.
fn write_app_key() -> std::path::PathBuf {
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("app-key-{}.pem", std::process::id()));
    std::fs::write(&path, sheltie_github::testing::rsa_pem()).unwrap();
    path
}

async fn fake_github(head: &str, signer: &OidcSigner) -> MockServer {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/.well-known/jwks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(signer.jwks()))
        .mount(&github)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/acme/widgets/check-runs/31"))
        .and(body_string_contains("Footprint: writes 1 target, reads 2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(check_run(head)))
        .expect(1)
        .mount(&github)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/installations/77/access_tokens"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "token": "ghs_e2e", "expires_at": "2099-01-01T00:00:00Z", "permissions": {}
        })))
        .mount(&github)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/widgets/check-runs"))
        .and(body_partial_json(
            json!({ "name": "Sheltie", "head_sha": head, "status": "queued" }),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(check_run(head)))
        .expect(1)
        .mount(&github)
        .await;
    github
}

fn check_run(head: &str) -> serde_json::Value {
    json!({
        "id": 31, "node_id": "CR", "details_url": null, "head_sha": head,
        "url": "https://api.github.com/repos/acme/widgets/check-runs/31", "html_url": null,
        "conclusion": null, "started_at": null, "completed_at": null, "name": "Sheltie",
        "pull_requests": [],
        "output": { "title": "t", "summary": "s", "text": null,
                    "annotations_count": 0, "annotations_url": "https://example.invalid" }
    })
}

/// Polls the fake GitHub until a request matching `pred` arrived.
async fn wait_for(github: &MockServer, what: &str, pred: impl Fn(&wiremock::Request) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let requests = github.received_requests().await.unwrap_or_default();
        if requests.iter().any(&pred) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[sqlx::test(migrations = false)]
async fn check_run_is_created_and_shows_the_uploaded_footprint(
    _: PgPoolOptions,
    database: PgConnectOptions,
) {
    let head = "1".repeat(40);
    let signer = OidcSigner::new("k1");
    let github = fake_github(&head, &signer).await;
    let key_path = write_app_key();
    let config = Config {
        database,
        bind: "127.0.0.1:0".parse().unwrap(),
        pool: PoolConfig::default(),
        agents: sheltie_server::AgentsConfig::default(),
        epoch: std::time::Duration::from_secs(1),
        github: Some(GitHubConfig {
            app_id: 123456,
            private_key_path: key_path.clone(),
            webhook_secret: WebhookSecret::new(SECRET).unwrap(),
            api_url: Some(github.uri()),
            oidc: OidcConfig {
                issuer: GITHUB_ACTIONS_ISSUER.into(),
                audience: "sheltie".into(),
                jwks_url: format!("{}/.well-known/jwks", github.uri()),
            },
        }),
    };
    let server = Server::start(config).await.unwrap();
    std::fs::remove_file(key_path).unwrap();
    let addr = server.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let handle = tokio::spawn(server.serve(async {
        stopped.await.ok();
    }));

    let body = std::fs::read(format!(
        "{}/../sheltie-github/tests/fixtures/pull_request.opened.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let response = reqwest::Client::new()
        .post(format!("http://{addr}/webhooks/github"))
        .header("content-type", "application/json")
        .header("x-github-event", "pull_request")
        .header("x-github-delivery", "e2e-1")
        .header(
            "x-hub-signature-256",
            WebhookSecret::new(SECRET).unwrap().sign(&body),
        )
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);

    // The webhook wakes the worker, which posts the check run.
    wait_for(&github, "the check run", |r| {
        r.method.as_str() == "POST" && r.url.path() == "/repos/acme/widgets/check-runs"
    })
    .await;

    // CI uploads the footprint; the check run is updated to show it.
    let report = json!({
        "adapter": "sheltie-cargo/test",
        "base": "0".repeat(40),
        "head": head,
        "base_tree": "e".repeat(40),
        "diff_hash": "sha256:abc",
        "footprint": { "writes": ["shop::cart"], "reads": ["shop::cart", "shop"], "tests": [], "leaky": false },
        "leak_reasons": []
    });
    let response = reqwest::Client::new()
        .post(format!("http://{addr}/v1/footprints"))
        .bearer_auth(signer.sign(&actions_claims("acme/widgets", 1001)))
        .json(&report)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
    wait_for(&github, "the check run update", |r| {
        r.method.as_str() == "PATCH"
    })
    .await;

    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("server did not shut down")
        .unwrap()
        .unwrap();
    // Dropping `github` verifies the check run was created and updated exactly once.
}

#[sqlx::test(migrations = false)]
async fn missing_private_key_fails_startup(_: PgPoolOptions, database: PgConnectOptions) {
    let config = Config {
        database,
        bind: "127.0.0.1:0".parse().unwrap(),
        pool: PoolConfig::default(),
        agents: sheltie_server::AgentsConfig::default(),
        epoch: std::time::Duration::from_secs(1),
        github: Some(GitHubConfig {
            app_id: 1,
            private_key_path: "/nonexistent/sheltie-app.pem".into(),
            webhook_secret: WebhookSecret::new(SECRET).unwrap(),
            api_url: None,
            oidc: OidcConfig::github_actions("sheltie"),
        }),
    };
    let err = Server::start(config).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("reading GitHub App private key"),
        "{err:#}"
    );
}

/// M3a: once approved and green with a footprint, the change becomes ready, the shadow
/// scheduler predicts it, and the check run shows the prediction.
#[sqlx::test(migrations = false)]
async fn ready_pull_request_shows_a_shadow_prediction(
    _: PgPoolOptions,
    database: PgConnectOptions,
) {
    let head = "1".repeat(40);
    let signer = OidcSigner::new("k1");
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/.well-known/jwks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(signer.jwks()))
        .mount(&github)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/installations/77/access_tokens"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "token": "ghs_e2e", "expires_at": "2099-01-01T00:00:00Z", "permissions": {}
        })))
        .mount(&github)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/widgets/check-runs"))
        .respond_with(ResponseTemplate::new(201).set_body_json(check_run(&head)))
        .mount(&github)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/acme/widgets/check-runs/31"))
        .respond_with(ResponseTemplate::new(200).set_body_json(check_run(&head)))
        .mount(&github)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": { "repository": { "pullRequest": {
            "headRefOid": head, "isDraft": false, "state": "OPEN", "reviewDecision": "APPROVED",
            "commits": { "nodes": [ { "commit": { "statusCheckRollup": { "contexts": {
                "pageInfo": { "hasNextPage": false },
                "nodes": [ { "__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": "SUCCESS" } ]
            } } } } ] }
        } } } })))
        .mount(&github)
        .await;

    let key_path = write_app_key();
    let config = Config {
        database,
        bind: "127.0.0.1:0".parse().unwrap(),
        pool: PoolConfig::default(),
        agents: sheltie_server::AgentsConfig::default(),
        epoch: Duration::from_millis(200),
        github: Some(GitHubConfig {
            app_id: 123456,
            private_key_path: key_path.clone(),
            webhook_secret: WebhookSecret::new(SECRET).unwrap(),
            api_url: Some(github.uri()),
            oidc: OidcConfig {
                issuer: GITHUB_ACTIONS_ISSUER.into(),
                audience: "sheltie".into(),
                jwks_url: format!("{}/.well-known/jwks", github.uri()),
            },
        }),
    };
    let server = Server::start(config).await.unwrap();
    std::fs::remove_file(key_path).unwrap();
    let addr = server.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let handle = tokio::spawn(server.serve(async {
        stopped.await.ok();
    }));
    let client = reqwest::Client::new();

    let body = std::fs::read(format!(
        "{}/../sheltie-github/tests/fixtures/pull_request.opened.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let response = client
        .post(format!("http://{addr}/webhooks/github"))
        .header("x-github-event", "pull_request")
        .header("x-github-delivery", "e2e-shadow-1")
        .header(
            "x-hub-signature-256",
            WebhookSecret::new(SECRET).unwrap().sign(&body),
        )
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);

    let report = json!({
        "adapter": "sheltie-cargo/test",
        "base": "0".repeat(40),
        "head": head,
        "base_tree": "e".repeat(40),
        "diff_hash": "sha256:abc",
        "footprint": { "writes": ["shop::cart"], "reads": ["shop::cart"], "tests": [], "leaky": false },
        "leak_reasons": []
    });
    let response = client
        .post(format!("http://{addr}/v1/footprints"))
        .bearer_auth(signer.sign(&actions_claims("acme/widgets", 1001)))
        .json(&report)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);

    // The base commit is not on the tracked branch yet, so the first prediction is
    // "reverify"; once main's history is known it becomes "would land now".
    wait_for(&github, "a reverify prediction", |r| {
        r.method.as_str() == "PATCH"
            && String::from_utf8_lossy(&r.body).contains("Shadow: would reverify")
    })
    .await;
    let push = std::fs::read(format!(
        "{}/../sheltie-github/tests/fixtures/push.main.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let mut push: serde_json::Value = serde_json::from_slice(&push).unwrap();
    push["commits"] = json!([{ "id": "0".repeat(40) }]);
    push["after"] = json!("0".repeat(40));
    let push = serde_json::to_vec(&push).unwrap();
    client
        .post(format!("http://{addr}/webhooks/github"))
        .header("x-github-event", "push")
        .header("x-github-delivery", "e2e-shadow-push")
        .header(
            "x-hub-signature-256",
            WebhookSecret::new(SECRET).unwrap().sign(&push),
        )
        .body(push)
        .send()
        .await
        .unwrap();
    wait_for(&github, "a land-now prediction", |r| {
        r.method.as_str() == "PATCH"
            && String::from_utf8_lossy(&r.body).contains("Shadow: would land now")
    })
    .await;

    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("server did not shut down")
        .unwrap()
        .unwrap();
}
