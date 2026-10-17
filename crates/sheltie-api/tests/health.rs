//! `GET /healthz` against a throwaway database (`#[sqlx::test]`, needs `DATABASE_URL`).

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use sheltie_store::Store;
use sqlx::PgPool;
use tower::ServiceExt;

async fn get_healthz(store: Store) -> (StatusCode, serde_json::Value) {
    let request = Request::get("/healthz").body(Body::empty()).unwrap();
    let response = sheltie_api::router(store, None, None, None)
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn healthy_when_database_answers(pool: PgPool) {
    let (status, body) = get_healthz(Store::from_pool(pool)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({ "status": "ok" }));
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn unavailable_when_database_is_gone(pool: PgPool) {
    pool.close().await;
    let (status, body) = get_healthz(Store::from_pool(pool)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, serde_json::json!({ "status": "unavailable" }));
}
