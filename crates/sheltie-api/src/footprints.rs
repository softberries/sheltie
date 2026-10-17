//! Uploads from CI, authenticated with GitHub Actions OIDC tokens:
//! `POST /v1/footprints` (a change's footprint), `POST /v1/merges` (a merge-engine report) and
//! `POST /v1/replays` (an intent-replay report) and `POST /v1/graphs` (the default branch's
//! build graph, for footprint prediction).
//!
//! Authenticated with a GitHub Actions OIDC token (`Authorization: Bearer <jwt>`). The token
//! names the repository; the upload is accepted only for repositories where the App is
//! installed, and only for that repository.

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use serde::Serialize;
use sheltie_core::{FootprintReport, MergeReport, ReplayReport};
use sheltie_github::oidc::OidcVerifier;
use sheltie_store::{MergeReportOutcome, ReplayReportOutcome};
use tokio::sync::Notify;

use crate::AppState;

/// Footprints are sets of names; 8 MB is far above any real workspace.
pub(crate) const MAX_UPLOAD_BYTES: usize = 8 * 1024 * 1024;
/// A whole build graph is larger: every target with its files and edges.
pub(crate) const MAX_GRAPH_BYTES: usize = 32 * 1024 * 1024;

/// Settings for accepting footprint uploads.
#[derive(Debug, Clone)]
pub struct Footprints {
    /// Verifies the uploader's OIDC token.
    pub verifier: Arc<OidcVerifier>,
    /// Notified after a footprint is stored, so check runs show it promptly.
    pub wake: Arc<Notify>,
}

#[derive(Debug, Serialize)]
pub(crate) struct UploadReply {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    changes: Option<u64>,
}

fn reply(code: StatusCode, status: &'static str) -> (StatusCode, Json<UploadReply>) {
    (
        code,
        Json(UploadReply {
            status,
            changes: None,
        }),
    )
}

pub(crate) async fn upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Json<UploadReply>) {
    let Some(footprints) = &state.footprints else {
        return reply(StatusCode::NOT_FOUND, "not_found");
    };
    let Some(token) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
    else {
        return reply(StatusCode::UNAUTHORIZED, "missing_token");
    };
    let claims = match footprints.verifier.verify(token.trim()).await {
        Ok(c) => c,
        Err(error) => {
            tracing::warn!(error = %error, "footprint upload: token rejected");
            return reply(StatusCode::UNAUTHORIZED, "invalid_token");
        }
    };

    let report: FootprintReport = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(error) => {
            tracing::warn!(repo = %claims.repository, error = %error, "footprint upload: malformed body");
            return reply(StatusCode::BAD_REQUEST, "invalid_body");
        }
    };
    if let Err(error) = report.validate() {
        tracing::warn!(repo = %claims.repository, error = %error, "footprint upload: invalid report");
        return reply(StatusCode::UNPROCESSABLE_ENTITY, "invalid_report");
    }

    match state.store.repository_installed(claims.repository_id).await {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(repo = %claims.repository, "footprint upload: App not installed on repository");
            return reply(StatusCode::FORBIDDEN, "repository_not_installed");
        }
        Err(error) => {
            tracing::error!(error = %error, "footprint upload: store failed");
            return reply(StatusCode::INTERNAL_SERVER_ERROR, "error");
        }
    }

    let uploaded_by = format!(
        "{} {} run {}",
        claims.repository, claims.event_name, claims.run
    );
    match state
        .store
        .save_footprint(claims.repository_id, &report, &uploaded_by)
        .await
    {
        Ok(changes) => {
            tracing::info!(
                repo = %claims.repository,
                head = %report.head,
                writes = report.footprint.writes.len(),
                reads = report.footprint.reads.len(),
                leaky = report.footprint.leaky,
                changes,
                "footprint stored"
            );
            footprints.wake.notify_one();
            (
                StatusCode::ACCEPTED,
                Json(UploadReply {
                    status: "accepted",
                    changes: Some(changes),
                }),
            )
        }
        Err(error) => {
            tracing::error!(error = %error, "footprint upload: store failed");
            reply(StatusCode::INTERNAL_SERVER_ERROR, "error")
        }
    }
}

/// `POST /v1/merges`: the merge engine reports how it merged a pull request onto the head.
pub(crate) async fn merge_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Json<UploadReply>) {
    let Some(footprints) = &state.footprints else {
        return reply(StatusCode::NOT_FOUND, "not_found");
    };
    let Some(token) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
    else {
        return reply(StatusCode::UNAUTHORIZED, "missing_token");
    };
    let claims = match footprints.verifier.verify(token.trim()).await {
        Ok(c) => c,
        Err(error) => {
            tracing::warn!(error = %error, "merge report: token rejected");
            return reply(StatusCode::UNAUTHORIZED, "invalid_token");
        }
    };
    let report: MergeReport = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(error) => {
            tracing::warn!(repo = %claims.repository, error = %error, "merge report: malformed body");
            return reply(StatusCode::BAD_REQUEST, "invalid_body");
        }
    };
    if report.validate().is_err() {
        return reply(StatusCode::UNPROCESSABLE_ENTITY, "invalid_report");
    }
    match state.store.repository_installed(claims.repository_id).await {
        Ok(true) => {}
        Ok(false) => return reply(StatusCode::FORBIDDEN, "repository_not_installed"),
        Err(error) => {
            tracing::error!(error = %error, "merge report: store failed");
            return reply(StatusCode::INTERNAL_SERVER_ERROR, "error");
        }
    }
    match state
        .store
        .record_merge_report(claims.repository_id, &report)
        .await
    {
        Ok(MergeReportOutcome::Stored) => {
            tracing::info!(repo = %claims.repository, pr = report.pr.0, resolved = report.result.is_some(), tier = %report.tier(), "merge report stored");
            footprints.wake.notify_one();
            reply(StatusCode::ACCEPTED, "accepted")
        }
        Ok(MergeReportOutcome::UnknownChange) => {
            reply(StatusCode::CONFLICT, "not_waiting_for_a_merge")
        }
        Err(error) => {
            tracing::error!(error = %error, "merge report: store failed");
            reply(StatusCode::INTERNAL_SERVER_ERROR, "error")
        }
    }
}

/// `POST /v1/replays`: the replay client reports how re-deriving a change went.
pub(crate) async fn replay_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Json<UploadReply>) {
    let Some(footprints) = &state.footprints else {
        return reply(StatusCode::NOT_FOUND, "not_found");
    };
    let Some(token) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
    else {
        return reply(StatusCode::UNAUTHORIZED, "missing_token");
    };
    let claims = match footprints.verifier.verify(token.trim()).await {
        Ok(c) => c,
        Err(error) => {
            tracing::warn!(error = %error, "replay report: token rejected");
            return reply(StatusCode::UNAUTHORIZED, "invalid_token");
        }
    };
    let report: ReplayReport = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(error) => {
            tracing::warn!(repo = %claims.repository, error = %error, "replay report: malformed body");
            return reply(StatusCode::BAD_REQUEST, "invalid_body");
        }
    };
    if report.validate().is_err() {
        return reply(StatusCode::UNPROCESSABLE_ENTITY, "invalid_report");
    }
    match state.store.repository_installed(claims.repository_id).await {
        Ok(true) => {}
        Ok(false) => return reply(StatusCode::FORBIDDEN, "repository_not_installed"),
        Err(error) => {
            tracing::error!(error = %error, "replay report: store failed");
            return reply(StatusCode::INTERNAL_SERVER_ERROR, "error");
        }
    }
    match state
        .store
        .record_replay_report(claims.repository_id, &report)
        .await
    {
        Ok(ReplayReportOutcome::Stored) => {
            tracing::info!(
                repo = %claims.repository, pr = report.pr.0, tier = %report.tier,
                passed = report.checks_passed(), cost_usd = report.cost_usd, "replay report stored"
            );
            footprints.wake.notify_one();
            reply(StatusCode::ACCEPTED, "accepted")
        }
        Ok(ReplayReportOutcome::UnknownChange) => {
            reply(StatusCode::CONFLICT, "not_waiting_for_a_replay")
        }
        Err(error) => {
            tracing::error!(error = %error, "replay report: store failed");
            reply(StatusCode::INTERNAL_SERVER_ERROR, "error")
        }
    }
}

/// Body of `POST /v1/graphs`.
#[derive(serde::Deserialize)]
struct GraphUpload {
    /// The default-branch commit the graph describes.
    commit: sheltie_core::CommitSha,
    /// The graph, as `sheltie-cargo graph` prints it.
    snapshot: serde_json::Value,
}

/// `POST /v1/graphs`: the adapter uploads the default branch's build graph after a push.
pub(crate) async fn graph_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Json<UploadReply>) {
    let Some(footprints) = &state.footprints else {
        return reply(StatusCode::NOT_FOUND, "not_found");
    };
    let Some(token) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
    else {
        return reply(StatusCode::UNAUTHORIZED, "missing_token");
    };
    let claims = match footprints.verifier.verify(token.trim()).await {
        Ok(c) => c,
        Err(error) => {
            tracing::warn!(error = %error, "graph upload: token rejected");
            return reply(StatusCode::UNAUTHORIZED, "invalid_token");
        }
    };
    let upload: GraphUpload = match serde_json::from_slice(&body) {
        Ok(u) => u,
        Err(error) => {
            tracing::warn!(repo = %claims.repository, error = %error, "graph upload: malformed body");
            return reply(StatusCode::BAD_REQUEST, "invalid_body");
        }
    };
    // Strictly typed before it is stored: unknown shapes are refused.
    if serde_json::from_value::<sheltie_graph::GraphSnapshot>(upload.snapshot.clone()).is_err() {
        return reply(StatusCode::UNPROCESSABLE_ENTITY, "invalid_graph");
    }
    match state.store.repository_installed(claims.repository_id).await {
        Ok(true) => {}
        Ok(false) => return reply(StatusCode::FORBIDDEN, "repository_not_installed"),
        Err(error) => {
            tracing::error!(error = %error, "graph upload: store failed");
            return reply(StatusCode::INTERNAL_SERVER_ERROR, "error");
        }
    }
    // Only the default branch describes the code everyone builds on: a run for a pull request
    // must not be able to replace it and skew every prediction.
    match state.store.default_branch(claims.repository_id).await {
        Ok(Some(branch))
            if claims.event_name == "push" && claims.git_ref == format!("refs/heads/{branch}") => {}
        Ok(_) => {
            tracing::warn!(repo = %claims.repository, event = %claims.event_name, git_ref = %claims.git_ref, "graph upload: not a push to the default branch");
            return reply(StatusCode::FORBIDDEN, "not_default_branch_push");
        }
        Err(error) => {
            tracing::error!(error = %error, "graph upload: store failed");
            return reply(StatusCode::INTERNAL_SERVER_ERROR, "error");
        }
    }
    let uploaded_by = format!(
        "{} {} run {}",
        claims.repository, claims.event_name, claims.run
    );
    match state
        .store
        .save_graph(
            claims.repository_id,
            &upload.commit,
            &upload.snapshot,
            &uploaded_by,
        )
        .await
    {
        Ok(()) => {
            tracing::info!(repo = %claims.repository, commit = %upload.commit, "graph stored");
            reply(StatusCode::ACCEPTED, "accepted")
        }
        Err(error) => {
            tracing::error!(error = %error, "graph upload: store failed");
            reply(StatusCode::INTERNAL_SERVER_ERROR, "error")
        }
    }
}
