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
    /// Tree id (the fake has no contents; a tree is just an id).
    pub tree: String,
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
    /// CI of a commit when it runs again on a `sheltie/` branch pointing at it (a probe), per
    /// commit (`SUCCESS`, `FAILURE`); no entry means that run has not happened yet.
    pub rerun_ci: HashMap<String, String>,
    /// CI state given to new merge commits on verify branches (default `SUCCESS`).
    pub verify_ci: Option<String>,
    /// PR heads that break CI on any verify or speculation build that contains them.
    pub failing_heads: BTreeSet<String>,
    /// Comments posted, as (PR number, body).
    pub comments: Vec<(u64, String)>,
    /// Pull requests opened through the API, as (number, title, head branch).
    pub opened: Vec<(u64, String, String)>,
    /// `repository_dispatch` events fired, as (event type, client payload).
    pub dispatches: Vec<(String, Value)>,
    /// Files served by the contents API (any ref), by path.
    pub files: BTreeMap<String, String>,
    /// Labels added, as (PR number, label).
    pub labels: Vec<(u64, String)>,
    /// Labels defined in the repository, as (name, color).
    pub label_defs: Vec<(String, String)>,
    /// OAuth: the user who signs in with code `good`, and the repository ids they can access.
    pub oauth_user: Option<(String, Vec<u64>)>,
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
                tree: format!("tree-{sha}"),
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
            .route(
                "/repos/{o}/{r}/commits/{sha}/check-suites",
                get(check_suites),
            )
            .route("/repos/{o}/{r}/merges", post(merge))
            .route("/repos/{o}/{r}/compare/{spec}", get(compare))
            .route("/repos/{o}/{r}/pulls/{n}", get(pull))
            .route("/repos/{o}/{r}/pulls", post(open_pull))
            .route("/repos/{o}/{r}/dispatches", post(dispatch))
            .route("/repos/{o}/{r}/contents/{*path}", get(contents))
            .route("/repos/{o}/{r}/issues/{n}/labels", post(add_labels))
            .route("/repos/{o}/{r}/labels", post(create_label))
            .route("/login/oauth/access_token", post(oauth_token))
            .route("/user", get(oauth_user))
            .route("/user/installations", get(oauth_installations))
            .route(
                "/user/installations/{id}/repositories",
                get(oauth_repositories),
            )
            .route(
                "/repos/{o}/{r}/issues/{n}/labels/{name}",
                axum::routing::delete(remove_label),
            )
            .route("/repos/{o}/{r}/git/commits/{sha}", get(get_commit))
            .route("/repos/{o}/{r}/git/commits", post(create_commit))
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
    if base.starts_with("sheltie/verify/") || base.starts_with("sheltie/spec/") {
        let broken = st.failing_heads.iter().any(|h| st.reaches(&sha, h));
        let state = if broken {
            "FAILURE".into()
        } else {
            st.verify_ci.clone().unwrap_or_else(|| "SUCCESS".into())
        };
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

async fn open_pull(State(s): Shared, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    let mut st = lock(&s);
    let n = 1000 + st.opened.len() as u64;
    let title = body["title"].as_str().unwrap_or_default().to_owned();
    let head = body["head"].as_str().unwrap_or_default().to_owned();
    st.opened.push((n, title, head));
    (StatusCode::CREATED, Json(json!({ "number": n })))
}

async fn get_commit(
    State(s): Shared,
    Path((_, _, sha)): Path<(String, String, String)>,
) -> Response {
    match lock(&s).commits.get(&sha) {
        Some(c) => Json(json!({
            "sha": sha,
            "tree": { "sha": c.tree },
            "parents": c.parents.iter().map(|p| json!({ "sha": p })).collect::<Vec<_>>(),
        }))
        .into_response(),
        None => status(StatusCode::NOT_FOUND, "Not Found"),
    }
}

async fn create_commit(State(s): Shared, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    let mut st = lock(&s);
    let parents: Vec<String> = body["parents"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| p.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let refs: Vec<&str> = parents.iter().map(String::as_str).collect();
    let sha = st.commit(&refs, body["message"].as_str().unwrap_or_default());
    if let Some(tree) = body["tree"].as_str()
        && let Some(c) = st.commits.get_mut(&sha)
    {
        c.tree = tree.to_owned();
    }
    (StatusCode::CREATED, Json(json!({ "sha": sha })))
}

async fn contents(
    State(s): Shared,
    Path((_, _, path)): Path<(String, String, String)>,
) -> Response {
    use base64::Engine as _;
    match lock(&s).files.get(&path) {
        Some(text) => Json(json!({ "content": base64::engine::general_purpose::STANDARD.encode(text), "encoding": "base64" })).into_response(),
        None => status(StatusCode::NOT_FOUND, "Not Found"),
    }
}

async fn add_labels(
    State(s): Shared,
    Path((_, _, n)): Path<(String, String, u64)>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let mut st = lock(&s);
    for l in body["labels"].as_array().into_iter().flatten() {
        st.labels
            .push((n, l.as_str().unwrap_or_default().to_owned()));
    }
    Json(json!([]))
}

async fn oauth_token(State(s): Shared, Json(body): Json<Value>) -> Json<Value> {
    let ok = body["code"] == "good" && lock(&s).oauth_user.is_some();
    Json(if ok {
        json!({ "access_token": "ghu_fake", "token_type": "bearer" })
    } else {
        json!({ "error": "bad_verification_code" })
    })
}

fn oauth_ok(headers: &axum::http::HeaderMap) -> bool {
    headers.get("authorization").and_then(|v| v.to_str().ok()) == Some("Bearer ghu_fake")
}

async fn oauth_user(State(s): Shared, headers: axum::http::HeaderMap) -> Response {
    match (&lock(&s).oauth_user, oauth_ok(&headers)) {
        (Some((login, _)), true) => Json(json!({ "login": login })).into_response(),
        _ => status(StatusCode::UNAUTHORIZED, "Bad credentials"),
    }
}

async fn oauth_installations(headers: axum::http::HeaderMap) -> Response {
    if !oauth_ok(&headers) {
        return status(StatusCode::UNAUTHORIZED, "Bad credentials");
    }
    Json(json!({ "total_count": 1, "installations": [{ "id": 77 }] })).into_response()
}

async fn oauth_repositories(State(s): Shared, headers: axum::http::HeaderMap) -> Response {
    if !oauth_ok(&headers) {
        return status(StatusCode::UNAUTHORIZED, "Bad credentials");
    }
    let repos: Vec<Value> = lock(&s)
        .oauth_user
        .as_ref()
        .map(|(_, ids)| ids.iter().map(|id| json!({ "id": id })).collect())
        .unwrap_or_default();
    Json(json!({ "total_count": repos.len(), "repositories": repos })).into_response()
}

async fn create_label(State(s): Shared, Json(body): Json<Value>) -> Response {
    let mut st = lock(&s);
    let name = body["name"].as_str().unwrap_or_default().to_owned();
    if st.label_defs.iter().any(|(n, _)| *n == name) {
        return status(StatusCode::UNPROCESSABLE_ENTITY, "Validation Failed");
    }
    st.label_defs
        .push((name, body["color"].as_str().unwrap_or_default().to_owned()));
    (StatusCode::CREATED, Json(json!({}))).into_response()
}

async fn remove_label(
    State(s): Shared,
    Path((_, _, n, name)): Path<(String, String, u64, String)>,
) -> Response {
    let mut st = lock(&s);
    let before = st.labels.len();
    st.labels.retain(|(pr, l)| !(*pr == n && *l == name));
    if st.labels.len() == before {
        return status(StatusCode::NOT_FOUND, "Label does not exist");
    }
    Json(json!([])).into_response()
}

async fn dispatch(State(s): Shared, Json(body): Json<Value>) -> StatusCode {
    let event = body["event_type"].as_str().unwrap_or_default().to_owned();
    lock(&s)
        .dispatches
        .push((event, body["client_payload"].clone()));
    StatusCode::NO_CONTENT
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

/// Check suites of a commit: one GitHub Actions suite per `sheltie/` branch at it whose run
/// finished (`rerun_ci`).
async fn check_suites(
    State(s): Shared,
    Path((_o, _r, sha)): Path<(String, String, String)>,
) -> Json<Value> {
    let st = lock(&s);
    let suites: Vec<Value> = match st.rerun_ci.get(&sha) {
        None => Vec::new(),
        Some(state) => st
            .refs
            .iter()
            .filter(|(name, at)| name.starts_with("sheltie/") && **at == sha)
            .map(|(name, _)| {
                json!({
                    "head_branch": name, "status": "completed",
                    "conclusion": state.to_lowercase(), "latest_check_runs_count": 1,
                    "app": { "id": 15368 }
                })
            })
            .collect(),
    };
    Json(json!({ "total_count": suites.len(), "check_suites": suites }))
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
    // One context standing for the commit's CI state.
    let rollup = st.ci.get(oid).map(|state| {
        let context = match state.as_str() {
            "SUCCESS" | "FAILURE" => {
                json!({ "__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": state })
            }
            "PENDING" => {
                json!({ "__typename": "CheckRun", "name": "test", "status": "IN_PROGRESS", "conclusion": null })
            }
            other => json!({ "__typename": "StatusContext", "state": other }),
        };
        json!({ "contexts": { "pageInfo": { "hasNextPage": false }, "nodes": [context] } })
    });
    Json(json!({ "data": { "repository": { "object": { "statusCheckRollup": rollup } } } }))
}
