//! Parsing webhook payloads into [`Intake`] events.
//!
//! Payloads come from GitHub but are still treated as untrusted input: only the fields Sheltie
//! uses are read, and each is validated into a domain type. Unknown fields are ignored so
//! GitHub can add fields without breaking intake.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use sheltie_core::intake::{
    ChecksEvent, InstallationEvent, Intake, IntentField, PrEvent, PrEventKind, PushEvent, RepoRef,
};
use sheltie_core::{CommitSha, InstallationId, Login, PrNumber, RepoFullName, RepoId};

/// A payload Sheltie handles but could not accept.
#[derive(Debug, thiserror::Error)]
pub enum PayloadError {
    /// Not valid JSON, or a required field is missing or has the wrong type.
    #[error("malformed {event} payload: {source}")]
    Json {
        /// The `X-GitHub-Event` name.
        event: String,
        /// The decoding error.
        #[source]
        source: serde_json::Error,
    },
    /// A field has an invalid value.
    #[error("invalid {event} payload: {reason}")]
    Invalid {
        /// The `X-GitHub-Event` name.
        event: String,
        /// What was wrong.
        reason: String,
    },
}

/// Parses a verified webhook body.
///
/// Returns `Ok(None)` for events and actions Sheltie does not act on (including `ping`).
pub fn parse(event: &str, body: &[u8]) -> Result<Option<Intake>, PayloadError> {
    let json = |source| PayloadError::Json {
        event: event.to_owned(),
        source,
    };
    let invalid = |reason: String| PayloadError::Invalid {
        event: event.to_owned(),
        reason,
    };
    match event {
        "pull_request" => {
            let p: PullRequestPayload = serde_json::from_slice(body).map_err(json)?;
            let kind = match p.action.as_str() {
                "opened" => PrEventKind::Opened,
                "synchronize" => PrEventKind::Synchronized,
                "reopened" => PrEventKind::Reopened,
                "closed" if p.pull_request.merged == Some(true) => PrEventKind::Merged,
                "closed" => PrEventKind::Closed,
                "ready_for_review" => PrEventKind::ReadyForReview,
                "edited" => PrEventKind::Edited,
                "converted_to_draft" => PrEventKind::ConvertedToDraft,
                _ => return Ok(None),
            };
            pr_event(kind, p.installation, p.repository, p.pull_request, p.sender)
                .map(Some)
                .map_err(invalid)
        }
        "pull_request_review" => {
            let p: ReviewPayload = serde_json::from_slice(body).map_err(json)?;
            let kind = match (
                p.action.as_str(),
                p.review.state.to_ascii_lowercase().as_str(),
            ) {
                ("submitted", "approved") => PrEventKind::Approved,
                ("submitted", "changes_requested") => PrEventKind::ChangesRequested,
                ("dismissed", _) => PrEventKind::ReviewDismissed,
                _ => return Ok(None),
            };
            pr_event(kind, p.installation, p.repository, p.pull_request, p.sender)
                .map(Some)
                .map_err(invalid)
        }
        "installation" => {
            let p: InstallationPayload = serde_json::from_slice(body).map_err(json)?;
            let installation = InstallationId(p.installation.id);
            let e = match p.action.as_str() {
                "created" => InstallationEvent::Created {
                    installation,
                    account: login(p.installation.account.login).map_err(invalid)?,
                    repos: repo_refs(p.repositories.unwrap_or_default()).map_err(invalid)?,
                },
                "deleted" => InstallationEvent::Deleted { installation },
                "suspend" => InstallationEvent::Suspended { installation },
                "unsuspend" => InstallationEvent::Unsuspended { installation },
                _ => return Ok(None),
            };
            Ok(Some(Intake::Installation(e)))
        }
        "installation_repositories" => {
            let p: InstallationReposPayload = serde_json::from_slice(body).map_err(json)?;
            if !matches!(p.action.as_str(), "added" | "removed") {
                return Ok(None);
            }
            Ok(Some(Intake::Installation(
                InstallationEvent::ReposChanged {
                    installation: InstallationId(p.installation.id),
                    account: login(p.installation.account.login).map_err(invalid)?,
                    added: repo_refs(p.repositories_added).map_err(invalid)?,
                    removed: p
                        .repositories_removed
                        .into_iter()
                        .map(|r| RepoId(r.id))
                        .collect(),
                },
            )))
        }
        "check_suite" | "check_run" => {
            let p: CheckPayload = serde_json::from_slice(body).map_err(json)?;
            let Some(check) = p.check_suite.or(p.check_run) else {
                return Err(invalid("missing check_suite / check_run".into()));
            };
            // Only finished checks change readiness; Sheltie's own check never does.
            if p.action != "completed"
                || check.name.as_deref() == Some(crate::check_run::CHECK_NAME)
            {
                return Ok(None);
            }
            checks_event(p.installation, p.repository, check.head_sha)
                .map(Some)
                .map_err(invalid)
        }
        "status" => {
            let p: StatusPayload = serde_json::from_slice(body).map_err(json)?;
            checks_event(p.installation, p.repository, p.sha)
                .map(Some)
                .map_err(invalid)
        }
        "push" => {
            let p: PushPayload = serde_json::from_slice(body).map_err(json)?;
            let Some(branch) = p.git_ref.strip_prefix("refs/heads/") else {
                return Ok(None); // tags
            };
            if p.deleted {
                return Ok(None);
            }
            let installation = p
                .installation
                .ok_or_else(|| invalid("missing installation".into()))?;
            let default_branch = p
                .repository
                .default_branch
                .clone()
                .ok_or_else(|| invalid("missing repository.default_branch".into()))?;
            Ok(Some(Intake::Push(PushEvent {
                installation: InstallationId(installation.id),
                branch: branch_name(branch.to_owned()).map_err(invalid)?,
                default_branch: branch_name(default_branch).map_err(invalid)?,
                after: CommitSha::parse(p.after).map_err(|e| invalid(e.to_string()))?,
                commits: p
                    .commits
                    .into_iter()
                    .map(|c| CommitSha::parse(c.id))
                    .collect::<Result<_, _>>()
                    .map_err(|e| invalid(e.to_string()))?,
                repo: repo_ref(p.repository).map_err(invalid)?,
            })))
        }
        _ => Ok(None),
    }
}

fn checks_event(
    installation: Option<InstallationRef>,
    repository: Repository,
    head: String,
) -> Result<Intake, String> {
    let installation = installation.ok_or("missing installation; is this an App webhook?")?;
    Ok(Intake::Checks(ChecksEvent {
        installation: InstallationId(installation.id),
        repo: repo_ref(repository)?,
        head: CommitSha::parse(head).map_err(|e| e.to_string())?,
    }))
}

/// Git branch names: no control characters or spaces, at most 255 bytes.
fn branch_name(name: String) -> Result<String, String> {
    let valid =
        (1..=255).contains(&name.len()) && !name.chars().any(|c| c.is_control() || c == ' ');
    if valid {
        Ok(name)
    } else {
        Err(format!("invalid branch name {name:?}"))
    }
}

fn pr_event(
    kind: PrEventKind,
    installation: Option<InstallationRef>,
    repository: Repository,
    pr: PullRequest,
    sender: User,
) -> Result<Intake, String> {
    let installation = installation.ok_or("missing installation; is this an App webhook?")?;
    let repository_default_branch = repository
        .default_branch
        .clone()
        .map(branch_name)
        .transpose()?;
    if pr.number == 0 {
        return Err("pull request number 0".into());
    }
    Ok(Intake::Pr(PrEvent {
        installation: InstallationId(installation.id),
        repo: repo_ref(repository)?,
        pr: PrNumber(pr.number),
        head: CommitSha::parse(pr.head.sha).map_err(|e| e.to_string())?,
        base: CommitSha::parse(pr.base.sha).map_err(|e| e.to_string())?,
        actor: login(sender.login)?,
        kind,
        at: pr.updated_at,
        base_ref: branch_name(pr.base.git_ref)?,
        default_branch: repository_default_branch,
        // Every pull request payload carries the current description.
        intent: IntentField::from_description(pr.body.as_deref()),
        merge_commit: match (kind, pr.merge_commit_sha) {
            (PrEventKind::Merged, Some(sha)) => {
                Some(CommitSha::parse(sha).map_err(|e| e.to_string())?)
            }
            _ => None,
        },
    }))
}

fn repo_ref(r: Repository) -> Result<RepoRef, String> {
    Ok(RepoRef {
        id: RepoId(r.id),
        full_name: RepoFullName::parse(r.full_name).map_err(|e| e.to_string())?,
    })
}

fn repo_refs(repos: Vec<Repository>) -> Result<Vec<RepoRef>, String> {
    repos.into_iter().map(repo_ref).collect()
}

/// GitHub logins are at most 39 characters; bots add a `[bot]` suffix.
fn login(value: String) -> Result<Login, String> {
    let valid = (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'[' | b']'));
    if valid {
        Ok(Login::new(value))
    } else {
        Err(format!("invalid login {value:?}"))
    }
}

#[derive(Deserialize)]
struct PullRequestPayload {
    action: String,
    pull_request: PullRequest,
    repository: Repository,
    installation: Option<InstallationRef>,
    sender: User,
}

#[derive(Deserialize)]
struct ReviewPayload {
    action: String,
    review: Review,
    pull_request: PullRequest,
    repository: Repository,
    installation: Option<InstallationRef>,
    sender: User,
}

#[derive(Deserialize)]
struct InstallationPayload {
    action: String,
    installation: Installation,
    repositories: Option<Vec<Repository>>,
}

#[derive(Deserialize)]
struct InstallationReposPayload {
    action: String,
    installation: Installation,
    #[serde(default)]
    repositories_added: Vec<Repository>,
    #[serde(default)]
    repositories_removed: Vec<RepositoryId>,
}

#[derive(Deserialize)]
struct PullRequest {
    number: u64,
    head: GitRef,
    base: GitRef,
    merged: Option<bool>,
    #[serde(default)]
    merge_commit_sha: Option<String>,
    updated_at: DateTime<Utc>,
    #[serde(default)]
    body: Option<String>,
}

#[derive(Deserialize)]
struct GitRef {
    sha: String,
    #[serde(rename = "ref")]
    git_ref: String,
}

#[derive(Deserialize)]
struct CheckPayload {
    action: String,
    check_suite: Option<Check>,
    check_run: Option<Check>,
    repository: Repository,
    installation: Option<InstallationRef>,
}

#[derive(Deserialize)]
struct Check {
    head_sha: String,
    /// Check runs have a name; check suites do not.
    name: Option<String>,
}

#[derive(Deserialize)]
struct StatusPayload {
    sha: String,
    repository: Repository,
    installation: Option<InstallationRef>,
}

#[derive(Deserialize)]
struct PushPayload {
    #[serde(rename = "ref")]
    git_ref: String,
    after: String,
    #[serde(default)]
    deleted: bool,
    #[serde(default)]
    commits: Vec<PushCommit>,
    repository: Repository,
    installation: Option<InstallationRef>,
}

#[derive(Deserialize)]
struct PushCommit {
    id: String,
}

#[derive(Deserialize)]
struct Review {
    state: String,
}

#[derive(Deserialize)]
struct Repository {
    id: u64,
    full_name: String,
    #[serde(default)]
    default_branch: Option<String>,
}

#[derive(Deserialize)]
struct RepositoryId {
    id: u64,
}

#[derive(Deserialize)]
struct InstallationRef {
    id: u64,
}

#[derive(Deserialize)]
struct Installation {
    id: u64,
    account: User,
}

#[derive(Deserialize)]
struct User {
    login: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_events_and_actions_are_ignored() {
        assert!(
            parse("ping", br#"{"zen":"Keep it logically awesome."}"#)
                .unwrap()
                .is_none()
        );
        assert!(parse("deployment", b"{}").unwrap().is_none());
        let labeled = br#"{"action":"labeled","pull_request":{"number":1,"head":{"sha":"","ref":"x"},"base":{"sha":"","ref":"main"},"updated_at":"2026-01-01T00:00:00Z"},"repository":{"id":1,"full_name":"a/b"},"sender":{"login":"x"}}"#;
        assert!(parse("pull_request", labeled).unwrap().is_none());
    }

    #[test]
    fn the_description_becomes_the_intent() {
        use sheltie_core::intake::IntentField;
        let event = |action: &str, body: &str| {
            let json = format!(
                r#"{{"action":"{action}","number":1,"pull_request":{{"number":1,"merged":false,"body":{body},"head":{{"sha":"{h}","ref":"x"}},"base":{{"sha":"{h}","ref":"main"}},"updated_at":"2026-01-01T00:00:00Z"}},"repository":{{"id":1,"full_name":"a/b"}},"installation":{{"id":2}},"sender":{{"login":"x"}}}}"#,
                h = "a".repeat(40)
            );
            match parse("pull_request", json.as_bytes()).unwrap() {
                Some(Intake::Pr(e)) => (e.kind, e.intent),
                other => panic!("{other:?}"),
            }
        };
        let block = serde_json::to_string("```sheltie-intent\n{\"task\":\"x\"}\n```").unwrap();
        assert!(matches!(
            event("opened", &block),
            (PrEventKind::Opened, IntentField::Valid(_))
        ));
        assert!(matches!(
            event("edited", &block),
            (PrEventKind::Edited, IntentField::Valid(_))
        ));
        assert_eq!(event("opened", "null").1, IntentField::Missing);
        assert!(matches!(
            event("edited", "\"```sheltie-intent\\n{\""),
            (_, IntentField::Invalid(_))
        ));
    }

    #[test]
    fn malformed_json_is_an_error() {
        assert!(matches!(
            parse("pull_request", b"{"),
            Err(PayloadError::Json { .. })
        ));
    }

    #[test]
    fn login_validation() {
        assert!(login("dependabot[bot]".into()).is_ok());
        assert!(login("a-b_c".into()).is_ok());
        for bad in ["", "a b", "x/../y", &"a".repeat(65)] {
            assert!(login(bad.into()).is_err(), "{bad:?}");
        }
    }
}
