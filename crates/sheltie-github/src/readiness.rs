//! Readiness from GitHub: approval, other checks, draft, read with one GraphQL query.
//!
//! GitHub applies the repository's branch protection when it computes `reviewDecision`, so
//! Sheltie does not reimplement those rules (or need permission to read them).

use serde::Deserialize;
use sheltie_core::CommitSha;
use sheltie_store::Readiness;

use crate::check_run::CHECK_NAME;

/// The GraphQL query; variables `owner`, `name`, `number`.
pub(crate) const QUERY: &str = r#"
query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      headRefOid
      isDraft
      state
      reviewDecision
      commits(last: 1) {
        nodes {
          commit {
            statusCheckRollup {
              contexts(first: 100) {
                pageInfo { hasNextPage }
                nodes {
                  __typename
                  ... on CheckRun { name status conclusion }
                  ... on StatusContext { context state }
                }
              }
            }
          }
        }
      }
    }
  }
}"#;

/// The `data` of the query response.
#[derive(Debug, Deserialize)]
pub struct PullRequestData {
    repository: Option<RepositoryData>,
}

#[derive(Debug, Deserialize)]
struct RepositoryData {
    #[serde(rename = "pullRequest")]
    pull_request: Option<PullRequestNode>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequestNode {
    head_ref_oid: String,
    is_draft: bool,
    state: String,
    review_decision: Option<String>,
    commits: Nodes<CommitNode>,
}

#[derive(Debug, Deserialize)]
struct Nodes<T> {
    nodes: Vec<T>,
}

#[derive(Debug, Deserialize)]
struct CommitNode {
    commit: Commit,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Commit {
    status_check_rollup: Option<Rollup>,
}

#[derive(Debug, Deserialize)]
struct Rollup {
    contexts: Contexts,
}

/// A page of a commit's checks and statuses (shared with [`crate::ci`]).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Contexts {
    pub(crate) page_info: PageInfo,
    pub(crate) nodes: Vec<Context>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PageInfo {
    pub(crate) has_next_page: bool,
}

/// One check run or commit status.
#[derive(Debug, Deserialize)]
#[serde(tag = "__typename")]
pub(crate) enum Context {
    CheckRun {
        name: String,
        status: String,
        conclusion: Option<String>,
    },
    StatusContext {
        state: String,
    },
}

/// Reduces the query result to readiness for the change at `head`.
///
/// - Approved: `reviewDecision` is `APPROVED`, or null (no review required).
/// - Checks green: at least one check or status other than Sheltie's own, all of them
///   finished successfully (`SUCCESS`, `NEUTRAL` or `SKIPPED`). More than 100 contexts count
///   as not green, to stay on the safe side.
/// - A pull request that is no longer open, or whose head moved, is not ready.
pub fn evaluate(data: &PullRequestData, head: &CommitSha) -> Readiness {
    let Some(pr) = data
        .repository
        .as_ref()
        .and_then(|r| r.pull_request.as_ref())
    else {
        return Readiness {
            head_matches: false,
            approved: false,
            checks_green: false,
            draft: false,
        };
    };
    let approved = matches!(pr.review_decision.as_deref(), None | Some("APPROVED"));
    let contexts = pr
        .commits
        .nodes
        .last()
        .and_then(|c| c.commit.status_check_rollup.as_ref())
        .map(|r| &r.contexts);
    let checks_green = contexts.is_some_and(|c| {
        let others: Vec<&Context> = c
            .nodes
            .iter()
            .filter(|ctx| !matches!(ctx, Context::CheckRun { name, .. } if name == CHECK_NAME))
            .collect();
        !c.page_info.has_next_page
            && !others.is_empty()
            && others.iter().all(|ctx| match ctx {
                Context::CheckRun {
                    status, conclusion, ..
                } => {
                    status == "COMPLETED"
                        && matches!(
                            conclusion.as_deref(),
                            Some("SUCCESS" | "NEUTRAL" | "SKIPPED")
                        )
                }
                Context::StatusContext { state } => state == "SUCCESS",
            })
    });
    Readiness {
        head_matches: pr.head_ref_oid == head.as_str() && pr.state == "OPEN",
        approved,
        checks_green,
        draft: pr.is_draft,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    const HEAD: &str = "1111111111111111111111111111111111111111";

    fn data(review: Value, contexts: Value, draft: bool) -> PullRequestData {
        serde_json::from_value(json!({
            "repository": { "pullRequest": {
                "headRefOid": HEAD, "isDraft": draft, "state": "OPEN", "reviewDecision": review,
                "commits": { "nodes": [ { "commit": { "statusCheckRollup": {
                    "contexts": { "pageInfo": { "hasNextPage": false }, "nodes": contexts }
                } } } ] }
            } }
        }))
        .unwrap()
    }

    fn head() -> CommitSha {
        CommitSha::parse(HEAD).unwrap()
    }

    fn ci(conclusion: &str) -> Value {
        json!({ "__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": conclusion })
    }

    #[test]
    fn approved_and_green() {
        let r = evaluate(
            &data(json!("APPROVED"), json!([ci("SUCCESS")]), false),
            &head(),
        );
        assert_eq!(
            r,
            Readiness {
                head_matches: true,
                approved: true,
                checks_green: true,
                draft: false
            }
        );
    }

    #[test]
    fn no_review_required_counts_as_approved() {
        assert!(evaluate(&data(Value::Null, json!([ci("SUCCESS")]), false), &head()).approved);
        assert!(
            !evaluate(
                &data(json!("REVIEW_REQUIRED"), json!([ci("SUCCESS")]), false),
                &head()
            )
            .approved
        );
        assert!(
            !evaluate(
                &data(json!("CHANGES_REQUESTED"), json!([ci("SUCCESS")]), false),
                &head()
            )
            .approved
        );
    }

    #[test]
    fn sheltie_is_ignored_and_something_else_must_pass() {
        let sheltie = json!({ "__typename": "CheckRun", "name": "Sheltie", "status": "QUEUED", "conclusion": null });
        assert!(
            !evaluate(
                &data(json!("APPROVED"), json!([sheltie.clone()]), false),
                &head()
            )
            .checks_green
        );
        assert!(
            evaluate(
                &data(json!("APPROVED"), json!([sheltie, ci("SUCCESS")]), false),
                &head()
            )
            .checks_green
        );
    }

    #[test]
    fn failing_running_or_pending_checks_are_not_green() {
        let running = json!({ "__typename": "CheckRun", "name": "t", "status": "IN_PROGRESS", "conclusion": null });
        let pending_status =
            json!({ "__typename": "StatusContext", "context": "ci/x", "state": "PENDING" });
        for contexts in [
            json!([ci("FAILURE")]),
            json!([running]),
            json!([pending_status]),
            json!([]),
        ] {
            assert!(
                !evaluate(&data(json!("APPROVED"), contexts.clone(), false), &head()).checks_green,
                "{contexts}"
            );
        }
        let ok_status =
            json!({ "__typename": "StatusContext", "context": "ci/x", "state": "SUCCESS" });
        assert!(
            evaluate(
                &data(json!("APPROVED"), json!([ok_status, ci("SKIPPED")]), false),
                &head()
            )
            .checks_green
        );
    }

    #[test]
    fn moved_head_or_missing_pr_is_not_ready() {
        let other = CommitSha::parse("2".repeat(40)).unwrap();
        assert!(
            !evaluate(
                &data(json!("APPROVED"), json!([ci("SUCCESS")]), false),
                &other
            )
            .head_matches
        );
        let missing: PullRequestData =
            serde_json::from_value(json!({ "repository": null })).unwrap();
        assert!(!evaluate(&missing, &head()).head_matches);
    }
}
