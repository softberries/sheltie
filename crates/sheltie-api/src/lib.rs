//! REST API (axum) and, from M7, the MCP server.
//!
//! The control plane is stateless: every handler reads and writes through [`Store`], so any
//! number of replicas can serve the same router.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use sheltie_core::DeliveryId;
use sheltie_github::{WebhookSecret, payload};
use sheltie_store::{Ingested, Store};
use tokio::sync::Notify;

mod footprints;

pub use footprints::Footprints;

/// GitHub caps webhook payloads at 25 MB.
const MAX_WEBHOOK_BYTES: usize = 25 * 1024 * 1024;

/// Settings for receiving GitHub webhooks.
#[derive(Debug, Clone)]
pub struct Webhooks {
    /// Secret configured on the GitHub App; every delivery must be signed with it.
    pub secret: WebhookSecret,
    /// Notified after an event is stored, so the check-run worker runs without waiting
    /// for its next poll.
    pub wake: Arc<Notify>,
}

#[derive(Clone)]
struct AppState {
    store: Store,
    webhooks: Option<Webhooks>,
    footprints: Option<Footprints>,
}

/// Builds the HTTP router. `POST /webhooks/github` is served only when `webhooks` is set,
/// `POST /v1/footprints` only when `footprints` is set.
pub fn router(store: Store, webhooks: Option<Webhooks>, footprints: Option<Footprints>) -> Router {
    let mut router = Router::new().route("/healthz", get(healthz));
    if webhooks.is_some() {
        router = router.route(
            "/webhooks/github",
            post(github_webhook).layer(DefaultBodyLimit::max(MAX_WEBHOOK_BYTES)),
        );
    }
    if footprints.is_some() {
        router = router
            .route(
                "/v1/footprints",
                post(footprints::upload).layer(DefaultBodyLimit::max(footprints::MAX_UPLOAD_BYTES)),
            )
            .route(
                "/v1/replays",
                post(footprints::replay_report)
                    .layer(DefaultBodyLimit::max(footprints::MAX_UPLOAD_BYTES)),
            )
            .route(
                "/v1/merges",
                post(footprints::merge_report)
                    .layer(DefaultBodyLimit::max(footprints::MAX_UPLOAD_BYTES)),
            );
    }
    router.with_state(AppState {
        store,
        webhooks,
        footprints,
    })
}

/// JSON body of every response: a short machine-readable status.
#[derive(Debug, Serialize)]
struct Status {
    status: &'static str,
}

fn reply(code: StatusCode, status: &'static str) -> (StatusCode, Json<Status>) {
    (code, Json(Status { status }))
}

/// Readiness: 200 when the database answers, 503 otherwise.
async fn healthz(State(state): State<AppState>) -> (StatusCode, Json<Status>) {
    match state.store.ping().await {
        Ok(()) => reply(StatusCode::OK, "ok"),
        Err(error) => {
            tracing::warn!(error = %error, "health check: database unavailable");
            reply(StatusCode::SERVICE_UNAVAILABLE, "unavailable")
        }
    }
}

/// Receives a GitHub webhook: verify, parse, store, wake the worker.
///
/// The signature is checked before anything else in the request is used. Bodies and
/// payload contents are never logged.
async fn github_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Json<Status>) {
    let Some(webhooks) = &state.webhooks else {
        return reply(StatusCode::NOT_FOUND, "not_found");
    };
    let signature = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok());
    if let Err(error) = webhooks.secret.verify(&body, signature) {
        tracing::warn!(error = %error, "rejected webhook");
        return reply(StatusCode::UNAUTHORIZED, "invalid_signature");
    }

    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let (Some(event), Some(delivery)) = (
        header("x-github-event").filter(|e| valid_event(e)),
        header("x-github-delivery").filter(|d| valid_delivery(d)),
    ) else {
        return reply(StatusCode::BAD_REQUEST, "missing_headers");
    };

    let intake = match payload::parse(event, &body) {
        Ok(Some(intake)) => intake,
        Ok(None) => {
            tracing::debug!(event, delivery, "webhook ignored");
            return reply(StatusCode::ACCEPTED, "ignored");
        }
        Err(error) => {
            tracing::warn!(event, delivery, error = %error, "webhook payload rejected");
            return reply(StatusCode::BAD_REQUEST, "invalid_payload");
        }
    };

    match state
        .store
        .ingest(&DeliveryId::new(delivery), event, &intake)
        .await
    {
        Ok(Ingested::Applied) => {
            tracing::info!(event, delivery, "webhook applied");
            webhooks.wake.notify_one();
            reply(StatusCode::ACCEPTED, "accepted")
        }
        Ok(Ingested::Duplicate) => {
            tracing::info!(event, delivery, "webhook redelivery ignored");
            reply(StatusCode::OK, "duplicate")
        }
        Err(error) => {
            tracing::error!(event, delivery, error = %error, "storing webhook failed");
            reply(StatusCode::INTERNAL_SERVER_ERROR, "error")
        }
    }
}

/// Event names are short lowercase identifiers such as `pull_request`.
fn valid_event(event: &str) -> bool {
    (1..=64).contains(&event.len()) && event.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

/// Delivery ids are GUIDs; accept any short id of printable ASCII without spaces.
fn valid_delivery(delivery: &str) -> bool {
    (1..=100).contains(&delivery.len()) && delivery.bytes().all(|b| b.is_ascii_graphic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_validation() {
        assert!(valid_event("pull_request_review"));
        assert!(!valid_event("Pull-Request"));
        assert!(!valid_event(""));
        assert!(valid_delivery("72d3162e-cc78-11e3-81ab-4c9367dc0958"));
        assert!(!valid_delivery("a b"));
        assert!(!valid_delivery(&"a".repeat(101)));
    }
}
