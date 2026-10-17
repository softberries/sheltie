//! GitHub App client: App JWT, cached installation tokens, check runs.
//!
//! Security-sensitive: holds the App private key. The key is only passed to octocrab's JWT
//! signer and never logged.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use chrono::Utc;
use octocrab::Octocrab;
use octocrab::models::{AppId, CheckRunId};
use octocrab::params::checks::{CheckRunConclusion, CheckRunOutput, CheckRunStatus};
use sheltie_core::{CommitSha, InstallationId, PrNumber, RepoFullName};

use crate::ci;
use crate::readiness::{self, PullRequestData};

use crate::check_run::{CHECK_NAME, CheckConclusion, CheckRunSpec, CheckStatus};

/// Time to open a connection to GitHub.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Time to send a request or read its response.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Errors from the GitHub client.
#[derive(Debug, thiserror::Error)]
pub enum GitHubError {
    /// The private key is not an RSA key in PEM format.
    #[error("invalid GitHub App private key")]
    InvalidKey,
    /// The API base URL is invalid.
    #[error("invalid GitHub API URL")]
    InvalidApiUrl,
    /// A GitHub API call failed.
    #[error("GitHub API request failed")]
    Api(#[from] octocrab::Error),
    /// GitHub answered in a way Sheltie does not expect.
    #[error("unexpected GitHub response: {0}")]
    Unexpected(String),
}

/// A GitHub App, authenticated with its private key.
///
/// Holds one client per installation so each installation's access token is cached and
/// refreshed shortly before it expires.
pub struct GitHubApp {
    app: Octocrab,
    /// This App's id, to tell its own check suites from the repository's CI.
    pub(crate) app_id: u64,
    installations: Mutex<HashMap<InstallationId, Arc<Octocrab>>>,
    api_url: String,
    /// `repo/label` pairs known to exist with Sheltie's color and description.
    pub(crate) labels_ready: Mutex<std::collections::HashSet<String>>,
}

impl GitHubApp {
    /// Creates the App client. `api_url` overrides `https://api.github.com`, for GitHub
    /// Enterprise Server or tests.
    pub fn new(
        app_id: u64,
        private_key_pem: &[u8],
        api_url: Option<&str>,
    ) -> Result<Self, GitHubError> {
        let key = octocrab::EncodingKey::from_rsa_pem(private_key_pem)
            .map_err(|_| GitHubError::InvalidKey)?;
        // A hung request would stall the worker loop that made it; installation clients share
        // these settings.
        let mut builder = Octocrab::builder()
            .app(AppId(app_id), key)
            .set_connect_timeout(Some(CONNECT_TIMEOUT))
            .set_read_timeout(Some(REQUEST_TIMEOUT))
            .set_write_timeout(Some(REQUEST_TIMEOUT));
        if let Some(url) = api_url {
            builder = builder
                .base_uri(url)
                .map_err(|_| GitHubError::InvalidApiUrl)?;
        }
        Ok(Self {
            app: builder.build()?,
            app_id,
            installations: Mutex::new(HashMap::new()),
            labels_ready: Mutex::new(std::collections::HashSet::new()),
            api_url: api_url
                .unwrap_or("https://api.github.com")
                .trim_end_matches('/')
                .to_owned(),
        })
    }

    /// The client for an installation. Octocrab clones copy the token cache, so clients
    /// are shared through `Arc` rather than cloned.
    fn installation(&self, id: InstallationId) -> Result<Arc<Octocrab>, GitHubError> {
        // The map stays consistent even if a holder panicked, so poisoning is ignored.
        let mut map = self
            .installations
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(client) = map.get(&id) {
            return Ok(Arc::clone(client));
        }
        let client = Arc::new(
            self.app
                .installation(octocrab::models::InstallationId(id.0))?,
        );
        map.insert(id, Arc::clone(&client));
        Ok(client)
    }

    /// Sends a request as the installation and returns the status and body, without turning
    /// error statuses into errors (callers decide what 409 or 422 mean).
    pub(crate) async fn raw(
        &self,
        installation: InstallationId,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<(u16, String), GitHubError> {
        let client = self.installation(installation)?;
        let uri = format!("{}{path}", self.api_url);
        let response = match method {
            "GET" => client._get(uri).await?,
            "POST" => client._post(uri, body.as_ref()).await?,
            "PATCH" => client._patch(uri, body.as_ref()).await?,
            "DELETE" => client._delete(uri, body.as_ref()).await?,
            other => {
                return Err(GitHubError::Unexpected(format!(
                    "unsupported method {other}"
                )));
            }
        };
        let status = response.status().as_u16();
        let text = client.body_to_string(response).await?;
        Ok((status, text))
    }

    /// Creates the "Sheltie" check run on `head`; returns its id.
    pub async fn create_check_run(
        &self,
        installation: InstallationId,
        repo: &RepoFullName,
        head: &CommitSha,
        external_id: &str,
        spec: &CheckRunSpec,
    ) -> Result<u64, GitHubError> {
        let client = self.installation(installation)?;
        let checks = client.checks(repo.owner(), repo.name());
        let mut request = checks
            .create_check_run(CHECK_NAME, head.as_str())
            .external_id(external_id)
            .output(output(spec));
        request = match spec.status {
            CheckStatus::Queued => request.status(CheckRunStatus::Queued),
            CheckStatus::InProgress => request.status(CheckRunStatus::InProgress),
            CheckStatus::Completed(c) => request
                .status(CheckRunStatus::Completed)
                .conclusion(conclusion(c))
                .completed_at(Utc::now()),
        };
        let run = request.send().await?;
        Ok(run.id.0)
    }

    /// Updates an existing check run to `spec`.
    pub async fn update_check_run(
        &self,
        installation: InstallationId,
        repo: &RepoFullName,
        check_run_id: u64,
        spec: &CheckRunSpec,
    ) -> Result<(), GitHubError> {
        let client = self.installation(installation)?;
        let checks = client.checks(repo.owner(), repo.name());
        let mut request = checks
            .update_check_run(CheckRunId(check_run_id))
            .output(output(spec));
        request = match spec.status {
            CheckStatus::Queued => request.status(CheckRunStatus::Queued),
            CheckStatus::InProgress => request.status(CheckRunStatus::InProgress),
            CheckStatus::Completed(c) => request
                .status(CheckRunStatus::Completed)
                .conclusion(conclusion(c))
                .completed_at(Utc::now()),
        };
        request.send().await?;
        Ok(())
    }
}

impl GitHubApp {
    /// Fetches what readiness needs about a pull request (see [`crate::readiness`]).
    pub async fn pull_request_readiness(
        &self,
        installation: InstallationId,
        repo: &RepoFullName,
        pr: PrNumber,
    ) -> Result<PullRequestData, GitHubError> {
        let client = self.installation(installation)?;
        let payload = serde_json::json!({
            "query": readiness::QUERY,
            "variables": { "owner": repo.owner(), "name": repo.name(), "number": pr.0 },
        });
        Ok(client.graphql(&payload).await?)
    }

    /// Fetches the CI rollup of a commit (see [`crate::ci`]).
    pub async fn commit_ci(
        &self,
        installation: InstallationId,
        repo: &RepoFullName,
        sha: &CommitSha,
    ) -> Result<ci::CommitData, GitHubError> {
        let client = self.installation(installation)?;
        let payload = serde_json::json!({
            "query": ci::QUERY,
            "variables": { "owner": repo.owner(), "name": repo.name(), "oid": sha.as_str() },
        });
        Ok(client.graphql(&payload).await?)
    }
}

impl fmt::Debug for GitHubApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GitHubApp").finish_non_exhaustive()
    }
}

fn output(spec: &CheckRunSpec) -> CheckRunOutput {
    CheckRunOutput {
        title: spec.title.clone(),
        summary: spec.summary.clone(),
        text: None,
        annotations: Vec::new(),
        images: Vec::new(),
    }
}

fn conclusion(c: CheckConclusion) -> CheckRunConclusion {
    match c {
        CheckConclusion::Success => CheckRunConclusion::Success,
        CheckConclusion::Neutral => CheckRunConclusion::Neutral,
        CheckConclusion::Failure => CheckRunConclusion::Failure,
    }
}
