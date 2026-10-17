//! CI state of a default-branch commit, from its checks and statuses.
//!
//! Sheltie's own check run is left out: it reports Sheltie's verdict on a pull request, not the
//! repository's CI, and a returned pull request's head can also be a commit on the branch.

use serde::Deserialize;
use sheltie_store::CiState;

use crate::check_run::CHECK_NAME;
use crate::readiness::{Context, Contexts};

/// The GraphQL query; variables `owner`, `name`, `oid`.
pub(crate) const QUERY: &str = r#"
query($owner: String!, $name: String!, $oid: GitObjectID!) {
  repository(owner: $owner, name: $name) {
    object(oid: $oid) {
      ... on Commit {
        statusCheckRollup {
          contexts(first: 100) {
            pageInfo { hasNextPage }
            nodes {
              __typename
              ... on CheckRun { name status conclusion }
              ... on StatusContext { state }
            }
          }
        }
      }
    }
  }
}"#;

/// The `data` of the query response.
#[derive(Debug, Deserialize)]
pub struct CommitData {
    repository: Option<Repo>,
}

#[derive(Debug, Deserialize)]
struct Repo {
    object: Option<Object>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Object {
    status_check_rollup: Option<Rollup>,
}

#[derive(Debug, Deserialize)]
struct Rollup {
    contexts: Contexts,
}

fn context_state(ctx: &Context) -> CiState {
    match ctx {
        Context::CheckRun {
            status, conclusion, ..
        } => match (status.as_str(), conclusion.as_deref()) {
            ("COMPLETED", Some("SUCCESS" | "NEUTRAL" | "SKIPPED")) => CiState::Success,
            ("COMPLETED", Some("STALE") | None) => CiState::Pending,
            ("COMPLETED", Some(_)) => CiState::Failure,
            _ => CiState::Pending,
        },
        Context::StatusContext { state } => match state.as_str() {
            "SUCCESS" => CiState::Success,
            "FAILURE" => CiState::Failure,
            "ERROR" => CiState::Error,
            _ => CiState::Pending,
        },
    }
}

/// The commit's CI: failure over error over pending over success, ignoring Sheltie's own
/// check. No checks means [`CiState::None`]; more than 100 can only be failing or pending.
pub fn evaluate(data: &CommitData) -> CiState {
    let Some(contexts) = data
        .repository
        .as_ref()
        .and_then(|r| r.object.as_ref())
        .and_then(|o| o.status_check_rollup.as_ref())
        .map(|r| &r.contexts)
    else {
        return CiState::None;
    };
    let states: Vec<CiState> = contexts
        .nodes
        .iter()
        .filter(|ctx| !matches!(ctx, Context::CheckRun { name, .. } if name == CHECK_NAME))
        .map(context_state)
        .collect();
    let any = |s: CiState| states.contains(&s);
    if any(CiState::Failure) {
        CiState::Failure
    } else if any(CiState::Error) {
        CiState::Error
    } else if any(CiState::Pending) || contexts.page_info.has_next_page {
        CiState::Pending
    } else if states.is_empty() {
        CiState::None
    } else {
        CiState::Success
    }
}

/// A check suite of a commit, as `GET /repos/{repo}/commits/{sha}/check-suites` lists it.
#[derive(Debug, Deserialize)]
pub(crate) struct Suite {
    head_branch: Option<String>,
    status: Option<String>,
    conclusion: Option<String>,
    #[serde(default)]
    latest_check_runs_count: u64,
    app: Option<SuiteApp>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SuiteApp {
    id: u64,
}

/// The CI of the runs triggered by pushing `branch`: `None` when none ran there yet. Suites of
/// this App and suites without check runs (GitHub opens one per subscribed App) are ignored.
pub(crate) fn branch_suites_state(suites: &[Suite], branch: &str, own_app: u64) -> Option<CiState> {
    let states: Vec<CiState> = suites
        .iter()
        .filter(|s| s.head_branch.as_deref() == Some(branch))
        .filter(|s| s.latest_check_runs_count > 0)
        .filter(|s| s.app.as_ref().is_none_or(|a| a.id != own_app))
        .map(|s| match (s.status.as_deref(), s.conclusion.as_deref()) {
            (Some("completed"), Some("success" | "neutral" | "skipped")) => CiState::Success,
            (Some("completed"), Some("stale") | None) => CiState::Pending,
            (Some("completed"), Some(_)) => CiState::Failure,
            _ => CiState::Pending,
        })
        .collect();
    if states.is_empty() {
        None
    } else if states.contains(&CiState::Failure) {
        Some(CiState::Failure)
    } else if states.contains(&CiState::Pending) {
        Some(CiState::Pending)
    } else {
        Some(CiState::Success)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn data(contexts: Value, more: bool) -> CommitData {
        serde_json::from_value(json!({ "repository": { "object": { "statusCheckRollup": {
            "contexts": { "pageInfo": { "hasNextPage": more }, "nodes": contexts }
        } } } }))
        .unwrap()
    }

    fn run(name: &str, status: &str, conclusion: Option<&str>) -> Value {
        json!({ "__typename": "CheckRun", "name": name, "status": status, "conclusion": conclusion })
    }

    fn status(state: &str) -> Value {
        json!({ "__typename": "StatusContext", "state": state })
    }

    #[test]
    fn states() {
        let ok = run("test", "COMPLETED", Some("SUCCESS"));
        let red = run("test", "COMPLETED", Some("FAILURE"));
        let running = run("test", "IN_PROGRESS", None);
        let state = |c: Value| evaluate(&data(c, false));
        assert_eq!(
            state(json!([
                ok.clone(),
                run("lint", "COMPLETED", Some("SKIPPED"))
            ])),
            CiState::Success
        );
        assert_eq!(state(json!([ok.clone(), red.clone()])), CiState::Failure);
        assert_eq!(state(json!([ok.clone(), status("ERROR")])), CiState::Error);
        assert_eq!(
            state(json!([ok.clone(), running.clone()])),
            CiState::Pending
        );
        assert_eq!(state(json!([red.clone(), running])), CiState::Failure);
        assert_eq!(state(json!([status("EXPECTED")])), CiState::Pending);
        assert_eq!(state(json!([])), CiState::None);
        assert_eq!(
            evaluate(&data(json!([ok.clone()]), true)),
            CiState::Pending,
            "more than a page"
        );
        let none: CommitData = serde_json::from_value(
            json!({ "repository": { "object": { "statusCheckRollup": null } } }),
        )
        .unwrap();
        assert_eq!(evaluate(&none), CiState::None);
    }

    #[test]
    fn sheltie_own_check_is_ignored() {
        let returned = run(CHECK_NAME, "COMPLETED", Some("ACTION_REQUIRED"));
        let ok = run("test", "COMPLETED", Some("SUCCESS"));
        assert_eq!(
            evaluate(&data(json!([returned.clone(), ok]), false)),
            CiState::Success
        );
        assert_eq!(evaluate(&data(json!([returned]), false)), CiState::None);
    }

    #[test]
    fn suites_on_a_branch() {
        let suites: Vec<Suite> = serde_json::from_value(json!([
            { "head_branch": "main", "status": "completed", "conclusion": "failure", "latest_check_runs_count": 2, "app": { "id": 15368 } },
            { "head_branch": "sheltie/bisect/abc", "status": "completed", "conclusion": "success", "latest_check_runs_count": 2, "app": { "id": 15368 } },
            { "head_branch": "sheltie/bisect/abc", "status": "queued", "conclusion": null, "latest_check_runs_count": 0, "app": { "id": 999 } },
            { "head_branch": "sheltie/bisect/abc", "status": "completed", "conclusion": "failure", "latest_check_runs_count": 1, "app": { "id": 7 } }
        ]))
        .unwrap();
        let state = |s: &[Suite]| branch_suites_state(s, "sheltie/bisect/abc", 7);
        assert_eq!(
            state(&suites),
            Some(CiState::Success),
            "own and empty suites ignored"
        );
        assert_eq!(branch_suites_state(&suites, "sheltie/bisect/zzz", 7), None);
        assert_eq!(state(&suites[..1]), None, "only the branch's runs count");
        let running: Vec<Suite> = serde_json::from_value(json!([
            { "head_branch": "sheltie/bisect/abc", "status": "in_progress", "conclusion": null, "latest_check_runs_count": 1, "app": { "id": 15368 } }
        ]))
        .unwrap();
        assert_eq!(state(&running), Some(CiState::Pending));
        assert_eq!(
            branch_suites_state(&suites, "main", 7),
            Some(CiState::Failure)
        );
    }
}
