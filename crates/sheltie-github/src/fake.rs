//! A stateful fake of the GitHub API for end-to-end tests: refs, merges with conflicts,
//! fast-forward-only ref updates, compare, check runs, comments and the GraphQL queries
//! Sheltie uses. Only built with the `test-support` feature.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::testing::OidcSigner;

/// A commit in the fake repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeCommit {
    /// Parent shas, first parent first.
    pub parents: Vec<String>,
    /// Commit message.
    pub message: String,
}

/// A pull request in the fake repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakePull {
    /// Title.
    pub title: String,
    /// Head commit.
    pub head: String,
    /// Approved and green, as the readiness query reports it.
    pub ready: bool,
}

/// The fake's state; tests read and change it through [`FakeGitHub::state`].
#[derive(Debug, Default)]
pub struct FakeState {
    /// All commits by sha.
    pub commits: HashMap<String, FakeCommit>,
    /// Branches.
    pub refs: BTreeMap<String, String>,
    /// Pull requests by number.
    pub pulls: BTreeMap<u64, FakePull>,
    /// Heads that conflict with anything they are merged into.
    pub conflicting: BTreeSet<String>,
    /// CI state per commit (`SUCCESS`, `FAILURE`, `PENDING`).
    pub ci: HashMap<String, String>,
    /// CI state given to new merge commits on verify branches (default `SUCCESS`).
    pub verify_ci: Option<String>,
    /// Comments posted, as (PR number, body).
    pub comments: Vec<(u64, String)>,
    /// Check runs: id → last request body.
    pub check_runs: BTreeMap<u64, Value>,
    /// If set, the next fast-forward of this branch first moves it to a new commit (an
    /// outside push), so the compare-and-swap loses once.
    pub race_on: Option<String>,
}

impl FakeState {
    /// Adds a commit and returns its sha.
    pub fn commit(&mut self, parents: &[&str], message: &str) -> String {
        let mut h = Sha256::new();
        h.update(message.as_bytes());
        for p in parents {
            h.update(p.as_bytes());
        }
        h.update(self.commits.len().to_le_bytes());
        let sha = hex::encode(&h.finalize()[..20]);
        self.commits.insert(
            sha.clone(),
            FakeCommit {
                parents: parents.iter().map(|p| (*p).to_owned()).collect(),
                message: message.to_owned(),
            },
        );
        sha
    }

    /// Whether `ancestor` is `sha` or reachable from it.
    pub fn reaches(&self, sha: &str, ancestor: &str) -> bool {
        let mut stack = vec![sha.to_owned()];
        let mut seen = BTreeSet::new();
        while let Some(s) = stack.pop() {
            if s == ancestor {
                return true;
            }
            if seen.insert(s.clone())
                && let Some(c) = self.commits.get(&s)
            {
                stack.extend(c.parents.iter().cloned());
            }
        }
        false
    }

    /// The first-parent chain of a branch, newest first.
    pub fn first_parents(&self, branch: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = self.refs.get(branch).cloned();
        while let Some(s) = cur {
            cur = self
                .commits
                .get(&s)
                .and_then(|c| c.parents.first().cloned());
            out.push(s);
        }
        out
    }
}

/// A running fake GitHub.
#[derive(Clone)]
pub struct FakeGitHub {
    state: Arc<Mutex<FakeState>>,
    uri: String,
}

impl FakeGitHub {
    /// Starts the fake with a `main` branch at a root commit; `signer` serves the OIDC keys.
    ///
    /// # Panics
    /// If no local port can be bound.
    pub async fn start(signer: &OidcSigner) -> Self {
        let mut state = FakeState::default();
        let root = state.commit(&[], "root");
        state.refs.insert("main".into(), root);
        let state = Arc::new(Mutex::new(state));
        let jwks = signer.jwks();
        let app = Router::new()
            .route("/.well-known/jwks", get(move || async move { Json(jwks) }))
            .route("/app/installations/{id}/access_tokens", post(token))
            .route("/repos/{o}/{r}/check-runs", post(create_check_run))
            .route("/repos/{o}/{r}/check-runs/{id}", patch(update_check_run))
            .route("/repos/{o}/{r}/git/ref/heads/{*branch}", get(get_ref))
            .route("/repos/{o}/{r}/git/refs", post(create_ref))
            .route(
                "/repos/{o}/{r}/git/refs/heads/{*branch}",
                patch(update_ref).delete(delete_ref),
            )
            .route("/repos/{o}/{r}/merges", post(merge))
            .route("/repos/{o}/{r}/compare/{spec}", get(compare))
            .route("/repos/{o}/{r}/pulls/{n}", get(pull))
            .route("/repos/{o}/{r}/issues/{n}/comments", post(comment))
            .route("/graphql", post(graphql))
            .with_state(Arc::clone(&state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|e| panic!("binding the fake GitHub: {e}"));
        let uri = format!(
            "http://{}",
            listener.local_addr().unwrap_or_else(|e| panic!("{e}"))
        );
        tokio::spawn(async move { axum::serve(listener, app).await });
        Self { state, uri }
    }

    /// Base URL to configure as the GitHub API URL.
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Locks the state for reading or changing it.
    pub fn state(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

type Shared = State<Arc<Mutex<FakeState>>>;

fn lock(s: &Arc<Mutex<FakeState>>) -> MutexGuard<'_, FakeState> {
    s.lock().unwrap_or_else(PoisonError::into_inner)
}

fn status(code: StatusCode, message: &str) -> Response {
    (code, Json(json!({ "message": message }))).into_response()
}

async fn token() -> Json<Value> {
    Json(json!({ "token": "ghs_fake", "expires_at": "2099-01-01T00:00:00Z", "permissions": {} }))
}

fn check_run_body(id: u64) -> Value {
    json!({
        "id": id, "node_id": "CR", "details_url": null, "head_sha": "0".repeat(40),
        "url": "https://example.invalid", "html_url": null, "conclusion": null,
        "started_at": null, "completed_at": null, "name": "Sheltie", "pull_requests": [],
        "output": { "title": "t", "summary": "s", "text": null, "annotations_count": 0,
                    "annotations_url": "https://example.invalid" }
    })
}

async fn create_check_run(State(s): Shared, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    let mut st = lock(&s);
    let id = st.check_runs.len() as u64 + 1;
    st.check_runs.insert(id, body);
    (StatusCode::CREATED, Json(check_run_body(id)))
}

async fn update_check_run(
    State(s): Shared,
    Path((_, _, id)): Path<(String, String, u64)>,
    Json(body): Json<Value>,
) -> Json<Value> {
    lock(&s).check_runs.insert(id, body);
    Json(check_run_body(id))
}

async fn get_ref(
    State(s): Shared,
    Path((_, _, branch)): Path<(String, String, String)>,
) -> Response {
    match lock(&s).refs.get(&branch) {
        Some(sha) => {
            Json(json!({ "ref": format!("refs/heads/{branch}"), "object": { "sha": sha } }))
                .into_response()
        }
        None => status(StatusCode::NOT_FOUND, "Not Found"),
    }
}

async fn create_ref(State(s): Shared, Json(body): Json<Value>) -> Response {
    let name = body["ref"]
        .as_str()
        .unwrap_or_default()
        .trim_start_matches("refs/heads/")
        .to_owned();
    let sha = body["sha"].as_str().unwrap_or_default().to_owned();
    let mut st = lock(&s);
    if st.refs.contains_key(&name) {
        return status(StatusCode::UNPROCESSABLE_ENTITY, "Reference already exists");
    }
    st.refs.insert(name, sha);
    StatusCode::CREATED.into_response()
}

async fn update_ref(
    State(s): Shared,
    Path((_, _, branch)): Path<(String, String, String)>,
    Json(body): Json<Value>,
) -> Response {
    let to = body["sha"].as_str().unwrap_or_default().to_owned();
    let force = body["force"].as_bool().unwrap_or(false);
    let mut st = lock(&s);
    if st.race_on.as_deref() == Some(branch.as_str()) {
        st.race_on = None;
        let current = st.refs.get(&branch).cloned().unwrap_or_default();
        let outside = st.commit(&[&current], "outside push");
        st.refs.insert(branch.clone(), outside);
    }
    let current = st.refs.get(&branch).cloned().unwrap_or_default();
    if !force && !st.reaches(&to, &current) {
        return status(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Update is not a fast forward",
        );
    }
    st.refs.insert(branch, to.clone());
    Json(json!({ "object": { "sha": to } })).into_response()
}

async fn delete_ref(
    State(s): Shared,
    Path((_, _, branch)): Path<(String, String, String)>,
) -> StatusCode {
    match lock(&s).refs.remove(&branch) {
        Some(_) => StatusCode::NO_CONTENT,
        None => StatusCode::UNPROCESSABLE_ENTITY,
    }
}

async fn merge(State(s): Shared, Json(body): Json<Value>) -> Response {
    let base = body["base"].as_str().unwrap_or_default().to_owned();
    let head = body["head"].as_str().unwrap_or_default().to_owned();
    let message = body["commit_message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let mut st = lock(&s);
    let Some(base_sha) = st.refs.get(&base).cloned() else {
        return status(StatusCode::NOT_FOUND, "Base does not exist");
    };
    if st.reaches(&base_sha, &head) {
        return StatusCode::NO_CONTENT.into_response();
    }
    if st.conflicting.contains(&head) {
        return status(StatusCode::CONFLICT, "Merge conflict");
    }
    let sha = st.commit(&[&base_sha, &head], &message);
    if base.starts_with("sheltie/verify/") {
        let state = st.verify_ci.clone().unwrap_or_else(|| "SUCCESS".into());
        st.ci.entry(sha.clone()).or_insert(state);
    }
    st.refs.insert(base, sha.clone());
    (StatusCode::CREATED, Json(json!({ "sha": sha }))).into_response()
}

async fn compare(State(s): Shared, Path((_, _, spec)): Path<(String, String, String)>) -> Response {
    let Some((base, head)) = spec.split_once("...") else {
        return status(StatusCode::NOT_FOUND, "bad compare");
    };
    let st = lock(&s);
    let mut commits = Vec::new();
    let mut stack = vec![head.to_owned()];
    let mut seen = BTreeSet::new();
    while let Some(sha) = stack.pop() {
        if !seen.insert(sha.clone()) || st.reaches(base, &sha) {
            continue;
        }
        if let Some(c) = st.commits.get(&sha) {
            commits.push(json!({ "sha": sha, "parents": c.parents.iter().map(|p| json!({ "sha": p })).collect::<Vec<_>>() }));
            stack.extend(c.parents.iter().cloned());
        }
    }
    Json(json!({ "total_commits": commits.len(), "commits": commits })).into_response()
}

async fn pull(State(s): Shared, Path((_, _, n)): Path<(String, String, u64)>) -> Response {
    match lock(&s).pulls.get(&n) {
        Some(p) => Json(json!({ "number": n, "title": p.title })).into_response(),
        None => status(StatusCode::NOT_FOUND, "Not Found"),
    }
}

async fn comment(
    State(s): Shared,
    Path((_, _, n)): Path<(String, String, u64)>,
    Json(body): Json<Value>,
) -> StatusCode {
    lock(&s)
        .comments
        .push((n, body["body"].as_str().unwrap_or_default().to_owned()));
    StatusCode::CREATED
}

async fn graphql(State(s): Shared, Json(body): Json<Value>) -> Json<Value> {
    let query = body["query"].as_str().unwrap_or_default();
    let vars = &body["variables"];
    let st = lock(&s);
    if query.contains("pullRequest") {
        let n = vars["number"].as_u64().unwrap_or_default();
        let Some(p) = st.pulls.get(&n) else {
            return Json(json!({ "data": { "repository": { "pullRequest": null } } }));
        };
        let (review, conclusion) = if p.ready {
            ("APPROVED", "SUCCESS")
        } else {
            ("REVIEW_REQUIRED", "FAILURE")
        };
        return Json(json!({ "data": { "repository": { "pullRequest": {
            "headRefOid": p.head, "isDraft": false, "state": "OPEN", "reviewDecision": review,
            "commits": { "nodes": [ { "commit": { "statusCheckRollup": { "contexts": {
                "pageInfo": { "hasNextPage": false },
                "nodes": [ { "__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": conclusion } ]
            } } } } ] }
        } } } }));
    }
    let oid = vars["oid"].as_str().unwrap_or_default();
    let rollup = st.ci.get(oid).map(|state| json!({ "state": state }));
    Json(json!({ "data": { "repository": { "object": { "statusCheckRollup": rollup } } } }))
}
