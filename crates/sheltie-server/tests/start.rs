//! M0 "done when": the server starts against a fresh database and reports healthy.
//!
//! Uses a throwaway database (`#[sqlx::test]`, needs `DATABASE_URL`) with no migrations applied,
//! so the test also covers the server migrating on startup.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use sheltie_server::{Config, Server};
use sheltie_store::PoolConfig;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::sync::oneshot;

#[sqlx::test(migrations = false)]
async fn server_starts_migrates_and_reports_healthy(_: PgPoolOptions, database: PgConnectOptions) {
    let config = Config {
        database,
        bind: "127.0.0.1:0".parse().unwrap(),
        pool: PoolConfig::default(),
        github: None,
    };
    let server = Server::start(config).await.unwrap();
    let addr = server.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let handle = tokio::spawn(server.serve(async {
        stopped.await.ok();
    }));

    let response = reqwest::get(format!("http://{addr}/healthz"))
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body, serde_json::json!({ "status": "ok" }));

    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("server did not shut down")
        .unwrap()
        .unwrap();
}
