//! Parsing webhook payloads into [`Intake`] events.
//!
//! Payloads come from GitHub but are still treated as untrusted input: only the fields Sheltie
//! uses are read, and each is validated into a domain type. Unknown fields are ignored so
//! GitHub can add fields without breaking intake.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use sheltie_core::intake::{InstallationEvent, Intake, PrEvent, PrEventKind, RepoRef};
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
        _ => Ok(None),
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
    updated_at: DateTime<Utc>,
}

#[derive(Deserialize)]
struct GitRef {
    sha: String,
}

#[derive(Deserialize)]
struct Review {
    state: String,
}

#[derive(Deserialize)]
struct Repository {
    id: u64,
    full_name: String,
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
        assert!(parse("push", b"{}").unwrap().is_none());
        let labeled = br#"{"action":"labeled","pull_request":{"number":1,"head":{"sha":""},"base":{"sha":""},"updated_at":"2026-01-01T00:00:00Z"},"repository":{"id":1,"full_name":"a/b"},"sender":{"login":"x"}}"#;
        assert!(parse("pull_request", labeled).unwrap().is_none());
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
