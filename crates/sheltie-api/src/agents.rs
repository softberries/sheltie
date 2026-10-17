//! Agent endpoints (M7): token authentication, `/v1/footprint`, `/v1/leases`,
//! `/v1/issues/safe`, and the MCP server at `/mcp`.
//!
//! Agents authenticate with a repository-scoped token (`Authorization: Bearer shl_…`) created
//! with `sheltie-server token create`. Only its SHA-256 is stored; tokens are never logged.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData, ServerHandler, tool, tool_handler, tool_router};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sheltie_core::{IssueNumber, RepoFullName, RepoId};
use sheltie_store::Store;

use crate::coordination::{CoordinationError, Coordinator};

/// The prefix of every agent token.
pub const TOKEN_PREFIX: &str = "shl_";

/// The SHA-256 of a token, as stored.
pub fn token_hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// The agent behind a request.
#[derive(Debug, Clone)]
pub struct Agent {
    /// The repository the token is scoped to.
    pub repo: RepoId,
    /// Its name.
    pub repo_name: RepoFullName,
    /// The token's name: who holds leases.
    pub name: String,
}

/// Settings for the agent endpoints.
#[derive(Debug, Clone)]
pub struct Agents {
    /// Predictions and leases.
    pub coordinator: Arc<Coordinator>,
    /// `Host` values the MCP endpoint accepts (DNS-rebinding protection); loopback is always
    /// accepted.
    pub mcp_hosts: Vec<String>,
}

/// Rejects requests without a live token; otherwise attaches the [`Agent`].
pub(crate) async fn authenticate(
    State(store): State<Store>,
    mut request: Request,
    next: Next,
) -> Response {
    let token = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(str::trim)
        .filter(|t| t.starts_with(TOKEN_PREFIX) && t.len() <= 200);
    let Some(token) = token else {
        return error(StatusCode::UNAUTHORIZED, "missing_token");
    };
    match store.token(&token_hash(token)).await {
        Ok(Some((repo, repo_name, name))) => {
            request.extensions_mut().insert(Agent {
                repo,
                repo_name,
                name,
            });
            next.run(request).await
        }
        Ok(None) => error(StatusCode::UNAUTHORIZED, "invalid_token"),
        Err(e) => {
            tracing::error!(error = %e, "token lookup failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "error")
        }
    }
}

fn error(code: StatusCode, status: &'static str) -> Response {
    (code, Json(serde_json::json!({ "status": status }))).into_response()
}

fn coordination_error(e: CoordinationError) -> Response {
    match e {
        CoordinationError::UnknownIssue => error(StatusCode::NOT_FOUND, "unknown_issue"),
        CoordinationError::Invalid(_) => error(StatusCode::BAD_REQUEST, "invalid_request"),
        CoordinationError::Store(e) => {
            tracing::error!(error = %e, "coordination failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "error")
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct FootprintQuery {
    /// Comma-separated paths.
    paths: Option<String>,
    issue: Option<u64>,
}

/// `GET /v1/footprint?paths=a,b` or `?issue=N`.
pub(crate) async fn footprint(
    State(agents): State<Agents>,
    axum::Extension(agent): axum::Extension<Agent>,
    Query(q): Query<FootprintQuery>,
) -> Response {
    let result = match (q.paths, q.issue) {
        (Some(paths), None) => {
            let paths: Vec<String> = paths.split(',').map(str::to_owned).collect();
            agents.coordinator.predict_paths(agent.repo, &paths).await
        }
        (None, Some(issue)) => {
            agents
                .coordinator
                .predict_issue(agent.repo, IssueNumber(issue))
                .await
        }
        _ => return error(StatusCode::BAD_REQUEST, "give_paths_or_issue"),
    };
    match result {
        Ok(p) => Json(p).into_response(),
        Err(e) => coordination_error(e),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeaseRequest {
    issue: u64,
}

/// `POST /v1/leases {"issue": N}`: 200 when held, 409 when taken or blocked.
pub(crate) async fn claim(
    State(agents): State<Agents>,
    axum::Extension(agent): axum::Extension<Agent>,
    Json(req): Json<LeaseRequest>,
) -> Response {
    match agents
        .coordinator
        .claim(agent.repo, IssueNumber(req.issue), &agent.name)
        .await
    {
        Ok(reply) if reply.holds() => Json(reply).into_response(),
        Ok(reply) => (StatusCode::CONFLICT, Json(reply)).into_response(),
        Err(e) => coordination_error(e),
    }
}

/// `DELETE /v1/leases/{issue}`.
pub(crate) async fn release(
    State(agents): State<Agents>,
    axum::Extension(agent): axum::Extension<Agent>,
    Path(issue): Path<u64>,
) -> Response {
    match agents
        .coordinator
        .release(agent.repo, IssueNumber(issue), &agent.name)
        .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => error(StatusCode::NOT_FOUND, "no_lease"),
        Err(e) => coordination_error(e),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct SafeQuery {
    limit: Option<usize>,
}

/// `GET /v1/issues/safe?limit=N`.
pub(crate) async fn safe_issues(
    State(agents): State<Agents>,
    axum::Extension(agent): axum::Extension<Agent>,
    Query(q): Query<SafeQuery>,
) -> Response {
    match agents
        .coordinator
        .safe_issues(agent.repo, q.limit.unwrap_or(10).min(100))
        .await
    {
        Ok(issues) => Json(issues).into_response(),
        Err(e) => coordination_error(e),
    }
}

/// The MCP server: the same operations as tools.
#[derive(Debug, Clone)]
pub struct SheltieMcp {
    coordinator: Arc<Coordinator>,
}

/// Arguments of `predict_footprint`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PredictArgs {
    /// Files the task will touch (relative to the repository root).
    #[serde(default)]
    pub paths: Option<Vec<String>>,
    /// Or an issue number, whose title and description are read.
    #[serde(default)]
    pub issue: Option<u64>,
}

/// Arguments naming an issue.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IssueArgs {
    /// The issue number.
    pub issue: u64,
}

/// Arguments of `safe_issues`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SafeArgs {
    /// Most issues to return (default 10, at most 100).
    #[serde(default)]
    pub limit: Option<usize>,
}

fn agent_of(parts: &axum::http::request::Parts) -> Result<Agent, ErrorData> {
    parts
        .extensions
        .get::<Agent>()
        .cloned()
        .ok_or_else(|| ErrorData::invalid_request("not authenticated", None))
}

fn tool_result<T: serde::Serialize>(
    r: Result<T, CoordinationError>,
) -> Result<CallToolResult, ErrorData> {
    match r {
        Ok(v) => Ok(CallToolResult::structured(
            serde_json::to_value(v).map_err(|e| ErrorData::internal_error(e.to_string(), None))?,
        )),
        Err(CoordinationError::Store(e)) => {
            tracing::error!(error = %e, "coordination failed");
            Err(ErrorData::internal_error("internal error", None))
        }
        Err(e) => Ok(CallToolResult::structured_error(
            serde_json::json!({ "error": e.to_string() }),
        )),
    }
}

#[tool_router]
impl SheltieMcp {
    /// Creates the server.
    pub fn new(coordinator: Arc<Coordinator>) -> Self {
        Self { coordinator }
    }

    #[tool(
        description = "Predict which build targets a task will write and affect, from the files it will touch or from an issue. Use it before starting work."
    )]
    async fn predict_footprint(
        &self,
        Extension(parts): Extension<axum::http::request::Parts>,
        Parameters(args): Parameters<PredictArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let agent = agent_of(&parts)?;
        tool_result(match (args.paths, args.issue) {
            (Some(paths), None) => self.coordinator.predict_paths(agent.repo, &paths).await,
            (None, Some(issue)) => {
                self.coordinator
                    .predict_issue(agent.repo, IssueNumber(issue))
                    .await
            }
            _ => Err(CoordinationError::Invalid(
                "give either paths or issue".into(),
            )),
        })
    }

    #[tool(
        description = "List open issues that can be worked on now without conflicting with work in progress or with each other."
    )]
    async fn safe_issues(
        &self,
        Extension(parts): Extension<axum::http::request::Parts>,
        Parameters(args): Parameters<SafeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let agent = agent_of(&parts)?;
        tool_result(
            self.coordinator
                .safe_issues(agent.repo, args.limit.unwrap_or(10).min(100))
                .await,
        )
    }

    #[tool(
        description = "Claim an issue before working on it. Status `granted` or `renewed`: go ahead (claim again to renew). `blocked` or `taken`: pick another issue."
    )]
    async fn claim_issue(
        &self,
        Extension(parts): Extension<axum::http::request::Parts>,
        Parameters(args): Parameters<IssueArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let agent = agent_of(&parts)?;
        tool_result(
            self.coordinator
                .claim(agent.repo, IssueNumber(args.issue), &agent.name)
                .await,
        )
    }

    #[tool(
        description = "Release your claim on an issue you will not finish. Claims also end when the pull request that fixes the issue lands."
    )]
    async fn release(
        &self,
        Extension(parts): Extension<axum::http::request::Parts>,
        Parameters(args): Parameters<IssueArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let agent = agent_of(&parts)?;
        tool_result(
            self.coordinator
                .release(agent.repo, IssueNumber(args.issue), &agent.name)
                .await
                .map(|released| serde_json::json!({ "released": released })),
        )
    }
}

#[tool_handler]
impl ServerHandler for SheltieMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Sheltie coordinates coding agents working on the same repository. Call safe_issues to find work, claim_issue before starting, and release if you stop. Reference the issue in your pull request (\"Fixes #N\") so the claim ends when it lands.",
        )
    }
}
