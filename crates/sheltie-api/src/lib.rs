//! REST API (axum) and, from M7, the MCP server.
//!
//! The control plane is stateless: every handler reads and writes through [`Store`], so any
//! number of replicas can serve the same router.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use sheltie_store::Store;

/// Builds the HTTP router.
pub fn router(store: Store) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .with_state(store)
}

/// Body of `GET /healthz`.
#[derive(Debug, Serialize)]
struct Health {
    status: &'static str,
}

/// Readiness: 200 when the database answers, 503 otherwise.
async fn healthz(State(store): State<Store>) -> (StatusCode, Json<Health>) {
    match store.ping().await {
        Ok(()) => (StatusCode::OK, Json(Health { status: "ok" })),
        Err(error) => {
            tracing::warn!(error = %error, "health check: database unavailable");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(Health {
                    status: "unavailable",
                }),
            )
        }
    }
}
