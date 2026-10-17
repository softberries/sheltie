//! `POST /webhooks/github` against a throwaway database.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use sheltie_api::{Webhooks, router};
use sheltie_core::{ChangeState, CommitSha, PrNumber, RepoId};
use sheltie_github::WebhookSecret;
use sheltie_store::Store;
use sqlx::PgPool;
use tokio::sync::Notify;
use tower::ServiceExt;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/../sheltie-github/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn secret() -> WebhookSecret {
    WebhookSecret::new("test-webhook-secret").unwrap()
}

struct Call<'a> {
    event: Option<&'a str>,
    delivery: Option<&'a str>,
    signature: Option<String>,
    body: Vec<u8>,
}

impl<'a> Call<'a> {
    fn signed(event: &'a str, delivery: &'a str, body: Vec<u8>) -> Self {
        Self {
            event: Some(event),
            delivery: Some(delivery),
            signature: Some(secret().sign(&body)),
            body,
        }
    }
}

async fn post(store: &Store, wake: &Arc<Notify>, call: Call<'_>) -> (StatusCode, String) {
    let webhooks = Webhooks {
        secret: secret(),
        wake: Arc::clone(wake),
    };
    let mut request = Request::post("/webhooks/github").header("content-type", "application/json");
    if let Some(e) = call.event {
        request = request.header("x-github-event", e);
    }
    if let Some(d) = call.delivery {
        request = request.header("x-github-delivery", d);
    }
    if let Some(s) = call.signature {
        request = request.header("x-hub-signature-256", s);
    }
    let response = router(store.clone(), Some(webhooks), None, None, None)
        .oneshot(request.body(Body::from(call.body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    (status, json["status"].as_str().unwrap().to_owned())
}

async fn state(store: &Store) -> Option<ChangeState> {
    let head = CommitSha::parse("1".repeat(40)).unwrap();
    store
        .change_state(RepoId(1001), PrNumber(7), &head)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn signed_event_is_stored_and_wakes_the_worker(pool: PgPool) {
    let store = Store::from_pool(pool);
    let wake = Arc::new(Notify::new());
    let call = Call::signed("pull_request", "d-1", fixture("pull_request.opened.json"));
    assert_eq!(
        post(&store, &wake, call).await,
        (StatusCode::ACCEPTED, "accepted".into())
    );
    assert_eq!(state(&store).await, Some(ChangeState::Pending));
    tokio::time::timeout(Duration::from_secs(1), wake.notified())
        .await
        .expect("worker was not woken");
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn redelivery_is_acknowledged_once(pool: PgPool) {
    let store = Store::from_pool(pool);
    let wake = Arc::new(Notify::new());
    let body = fixture("pull_request.opened.json");
    post(
        &store,
        &wake,
        Call::signed("pull_request", "d-1", body.clone()),
    )
    .await;
    let again = post(&store, &wake, Call::signed("pull_request", "d-1", body)).await;
    assert_eq!(again, (StatusCode::OK, "duplicate".into()));
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn unsigned_or_forged_requests_are_rejected_and_not_stored(pool: PgPool) {
    let store = Store::from_pool(pool);
    let wake = Arc::new(Notify::new());
    let body = fixture("pull_request.opened.json");

    let mut unsigned = Call::signed("pull_request", "d-1", body.clone());
    unsigned.signature = None;
    assert_eq!(
        post(&store, &wake, unsigned).await.0,
        StatusCode::UNAUTHORIZED
    );

    let mut forged = Call::signed("pull_request", "d-2", body.clone());
    forged.signature = Some(WebhookSecret::new("wrong").unwrap().sign(&body));
    assert_eq!(
        post(&store, &wake, forged).await.0,
        StatusCode::UNAUTHORIZED
    );

    let mut tampered = Call::signed("pull_request", "d-3", body.clone());
    tampered.body = body
        .iter()
        .map(|b| if *b == b'7' { b'8' } else { *b })
        .collect();
    assert_eq!(
        post(&store, &wake, tampered).await.0,
        StatusCode::UNAUTHORIZED
    );

    assert_eq!(state(&store).await, None);
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn missing_headers_and_bad_payloads_are_bad_requests(pool: PgPool) {
    let store = Store::from_pool(pool);
    let wake = Arc::new(Notify::new());

    let mut no_event = Call::signed("pull_request", "d-1", fixture("pull_request.opened.json"));
    no_event.event = None;
    assert_eq!(
        post(&store, &wake, no_event).await,
        (StatusCode::BAD_REQUEST, "missing_headers".into())
    );

    let bad = Call::signed("pull_request", "d-2", b"{\"action\":\"opened\"}".to_vec());
    assert_eq!(
        post(&store, &wake, bad).await,
        (StatusCode::BAD_REQUEST, "invalid_payload".into())
    );
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn ping_is_ignored(pool: PgPool) {
    let store = Store::from_pool(pool);
    let wake = Arc::new(Notify::new());
    let call = Call::signed("ping", "d-1", b"{\"zen\":\"Design for failure.\"}".to_vec());
    assert_eq!(
        post(&store, &wake, call).await,
        (StatusCode::ACCEPTED, "ignored".into())
    );
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn webhook_route_is_absent_without_configuration(pool: PgPool) {
    let response = router(Store::from_pool(pool), None, None, None, None)
        .oneshot(
            Request::post("/webhooks/github")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
