//! Forge events Sheltie acts on, independent of the webhook payload format.
//!
//! `sheltie-github` parses and validates webhooks into these types; `sheltie-store` applies them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{CommitSha, InstallationId, Login, PrNumber, RepoFullName, RepoId};

/// A repository as named in an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoRef {
    /// Stable numeric id.
    pub id: RepoId,
    /// `owner/name` at the time of the event.
    pub full_name: RepoFullName,
}

text_enum! {
    /// What happened to a pull request.
    pub enum PrEventKind {
        /// Opened.
        Opened => "opened",
        /// New commits pushed: the head moved.
        Synchronized => "synchronized",
        /// Reopened after being closed.
        Reopened => "reopened",
        /// Closed without merging.
        Closed => "closed",
        /// Merged outside Sheltie.
        Merged => "merged",
        /// Marked ready for review.
        ReadyForReview => "ready_for_review",
        /// Converted back to a draft.
        ConvertedToDraft => "converted_to_draft",
        /// A review approved it.
        Approved => "approved",
        /// A review requested changes.
        ChangesRequested => "changes_requested",
        /// A review was dismissed.
        ReviewDismissed => "review_dismissed",
        /// The title or description was edited (the intent may have changed).
        Edited => "edited",
    }
}

/// How an event affects the lifecycle of the PR's changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    /// The PR is open at the event's head: track that head, supersede older ones.
    Open,
    /// The PR is no longer open: close its live changes.
    Close,
    /// Recorded for scheduling decisions; no lifecycle change.
    Record,
}

impl PrEventKind {
    /// The lifecycle effect of this event.
    pub const fn lifecycle(self) -> Lifecycle {
        match self {
            PrEventKind::Opened | PrEventKind::Synchronized | PrEventKind::Reopened => {
                Lifecycle::Open
            }
            PrEventKind::Closed | PrEventKind::Merged => Lifecycle::Close,
            PrEventKind::ReadyForReview
            | PrEventKind::ConvertedToDraft
            | PrEventKind::Approved
            | PrEventKind::ChangesRequested
            | PrEventKind::ReviewDismissed
            | PrEventKind::Edited => Lifecycle::Record,
        }
    }
}

/// An event on one pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrEvent {
    /// The installation the event came through.
    pub installation: InstallationId,
    /// The repository.
    pub repo: RepoRef,
    /// The pull request.
    pub pr: PrNumber,
    /// Head commit at the time of the event.
    pub head: CommitSha,
    /// Base commit at the time of the event.
    pub base: CommitSha,
    /// Who triggered it.
    pub actor: Login,
    /// What happened.
    pub kind: PrEventKind,
    /// The forge's last-update time of the pull request; orders out-of-order deliveries.
    pub at: DateTime<Utc>,
    /// The branch the pull request targets, e.g. `main`.
    pub base_ref: String,
    /// The repository's default branch, when the payload names it.
    pub default_branch: Option<String>,
    /// For [`PrEventKind::Merged`]: the commit that landed on the target branch.
    pub merge_commit: Option<CommitSha>,
    /// The intent block of the description, when the event carries the description.
    pub intent: IntentField,
    /// Issues the description closes, when the event carries the description.
    #[serde(default)]
    pub closes: Option<std::collections::BTreeSet<crate::IssueNumber>>,
}

/// The intent block of a pull request description.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum IntentField {
    /// The event does not carry the description (reviews).
    #[default]
    Unknown,
    /// The description has no intent block.
    Missing,
    /// A valid intent.
    Valid(crate::Intent),
    /// An intent block that was rejected, and why.
    Invalid(String),
}

impl IntentField {
    /// Parses the description of a pull request.
    pub fn from_description(body: Option<&str>) -> Self {
        match crate::Intent::from_description(body.unwrap_or_default()) {
            Ok(Some(i)) => IntentField::Valid(i),
            Ok(None) => IntentField::Missing,
            Err(e) => IntentField::Invalid(e.0),
        }
    }
}

/// Checks or commit statuses on a commit changed; readiness of changes at `head` must be
/// re-evaluated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecksEvent {
    /// The installation the event came through.
    pub installation: InstallationId,
    /// The repository.
    pub repo: RepoRef,
    /// The commit whose checks changed.
    pub head: CommitSha,
}

/// Commits were pushed to a branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushEvent {
    /// The installation the event came through.
    pub installation: InstallationId,
    /// The repository.
    pub repo: RepoRef,
    /// The branch name (without `refs/heads/`).
    pub branch: String,
    /// The repository's default branch.
    pub default_branch: String,
    /// The new branch head.
    pub after: CommitSha,
    /// The pushed commits, oldest first (at most 2048, per GitHub).
    pub commits: Vec<CommitSha>,
}

/// A change to where the App is installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstallationEvent {
    /// The App was installed on an account, with access to these repositories.
    Created {
        /// The new installation.
        installation: InstallationId,
        /// The account that installed it.
        account: Login,
        /// Repositories the installation can access.
        repos: Vec<RepoRef>,
    },
    /// The App was uninstalled.
    Deleted {
        /// The removed installation.
        installation: InstallationId,
    },
    /// The installation was suspended; Sheltie must not act on its repositories.
    Suspended {
        /// The suspended installation.
        installation: InstallationId,
    },
    /// A suspended installation was resumed.
    Unsuspended {
        /// The resumed installation.
        installation: InstallationId,
    },
    /// Repositories were added to or removed from an installation.
    ReposChanged {
        /// The installation.
        installation: InstallationId,
        /// The account that owns the installation.
        account: Login,
        /// Newly accessible repositories.
        added: Vec<RepoRef>,
        /// Repositories no longer accessible.
        removed: Vec<RepoId>,
    },
}

/// Any event Sheltie acts on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Intake {
    /// A pull request event.
    Pr(PrEvent),
    /// An installation event.
    Installation(InstallationEvent),
    /// Checks or statuses changed on a commit.
    Checks(ChecksEvent),
    /// A push to a branch.
    Push(PushEvent),
    /// An issue event.
    Issue(IssueEvent),
}

/// What happened to an issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueEventKind {
    /// Opened.
    Opened,
    /// Title or description edited.
    Edited,
    /// Reopened.
    Reopened,
    /// Closed.
    Closed,
    /// Someone (an agent) was assigned: they claim it.
    Assigned(Login),
    /// Someone was unassigned: they release it.
    Unassigned(Login),
}

/// An event on one issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueEvent {
    /// The installation the event came through.
    pub installation: InstallationId,
    /// The repository.
    pub repo: RepoRef,
    /// The issue.
    pub issue: crate::IssueNumber,
    /// What happened.
    pub kind: IssueEventKind,
    /// Title.
    pub title: String,
    /// Description, truncated to [`IssueEvent::MAX_BODY`] characters.
    pub body: String,
    /// The forge's last-update time of the issue.
    pub at: DateTime<Utc>,
    /// The repository's default branch, when the payload names it.
    pub default_branch: Option<String>,
}

impl IssueEvent {
    /// Longest description kept.
    pub const MAX_BODY: usize = 65_536;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_open_and_close_events_change_lifecycle() {
        let open: Vec<_> = PrEventKind::ALL
            .iter()
            .filter(|k| k.lifecycle() == Lifecycle::Open)
            .map(|k| k.as_str())
            .collect();
        assert_eq!(open, ["opened", "synchronized", "reopened"]);
        let close: Vec<_> = PrEventKind::ALL
            .iter()
            .filter(|k| k.lifecycle() == Lifecycle::Close)
            .map(|k| k.as_str())
            .collect();
        assert_eq!(close, ["closed", "merged"]);
    }
}
