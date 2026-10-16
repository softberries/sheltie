//! M1 "done when": a pull request webhook leads to a "Sheltie" check run on its head.
//!
//! Runs the real server against a throwaway database and a fake GitHub (wiremock).

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use aws_lc_rs::encoding::{AsDer, Pkcs8V1Der};
use aws_lc_rs::rsa::{KeyPair, KeySize};
use base64::Engine as _;
use serde_json::json;
use sheltie_github::WebhookSecret;
use sheltie_server::{Config, GitHubConfig, Server};
use sheltie_store::PoolConfig;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::sync::oneshot;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SECRET: &str = "e2e-webhook-secret";

/// Writes a fresh App key (PKCS#8 PEM) under the test's scratch directory.
fn write_app_key() -> std::path::PathBuf {
    let key = KeyPair::generate(KeySize::Rsa2048).unwrap();
    let der = AsDer::<Pkcs8V1Der>::as_der(&key).unwrap();
    let b64 = base64::engine::general_purpose::STANDARD.encode(der.as_ref());
    let body: Vec<&str> = b64
        .as_bytes()
        .chunks(64)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect();
    let pem = format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        body.join("\n")
    );
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("app-key-{}.pem", std::process::id()));
    std::fs::write(&path, pem).unwrap();
    path
}

async fn fake_github(head: &str) -> MockServer {
    let github = MockServer::start().await;
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
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 31, "node_id": "CR", "details_url": null, "head_sha": head,
            "url": "https://api.github.com/repos/acme/widgets/check-runs/31", "html_url": null,
            "conclusion": null, "started_at": null, "completed_at": null, "name": "Sheltie",
            "pull_requests": [],
            "output": { "title": "t", "summary": "s", "text": null,
                        "annotations_count": 0, "annotations_url": "https://example.invalid" }
        })))
        .expect(1)
        .mount(&github)
        .await;
    github
}

#[sqlx::test(migrations = false)]
async fn pull_request_gets_a_sheltie_check_run(_: PgPoolOptions, database: PgConnectOptions) {
    let head = "1".repeat(40);
    let github = fake_github(&head).await;
    let key_path = write_app_key();
    let config = Config {
        database,
        bind: "127.0.0.1:0".parse().unwrap(),
        pool: PoolConfig::default(),
        github: Some(GitHubConfig {
            app_id: 123456,
            private_key_path: key_path.clone(),
            webhook_secret: WebhookSecret::new(SECRET).unwrap(),
            api_url: Some(github.uri()),
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
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let requests = github.received_requests().await.unwrap_or_default();
        if requests
            .iter()
            .any(|r| r.url.path() == "/repos/acme/widgets/check-runs")
        {
            break;
        }
        assert!(Instant::now() < deadline, "no check run was posted");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("server did not shut down")
        .unwrap()
        .unwrap();
    // Dropping `github` verifies the check run was created exactly once.
}

#[sqlx::test(migrations = false)]
async fn missing_private_key_fails_startup(_: PgPoolOptions, database: PgConnectOptions) {
    let config = Config {
        database,
        bind: "127.0.0.1:0".parse().unwrap(),
        pool: PoolConfig::default(),
        github: Some(GitHubConfig {
            app_id: 1,
            private_key_path: "/nonexistent/sheltie-app.pem".into(),
            webhook_secret: WebhookSecret::new(SECRET).unwrap(),
            api_url: None,
        }),
    };
    let err = Server::start(config).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("reading GitHub App private key"),
        "{err:#}"
    );
}
