//! What the "Sheltie" check run says for each change state.

use sheltie_core::{ChangeState, CommitSha};

/// Name of the check run Sheltie posts on every pull request head.
pub const CHECK_NAME: &str = "Sheltie";

/// Check run status as GitHub models it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    /// Waiting to start.
    Queued,
    /// Running.
    InProgress,
    /// Finished with a conclusion.
    Completed(CheckConclusion),
}

/// Conclusion of a completed check run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckConclusion {
    /// Landed.
    Success,
    /// Not landed, but not a failure of the PR: closed or superseded.
    Neutral,
    /// Sheltie could not land it; the author or an agent must act.
    Failure,
}

/// The content of a check run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRunSpec {
    /// Status and, when completed, conclusion.
    pub status: CheckStatus,
    /// One-line title shown next to the check.
    pub title: String,
    /// Markdown summary on the check's page.
    pub summary: String,
}

/// Renders the check run for a change in `state` at `head`.
pub fn spec_for(state: ChangeState, head: &CommitSha) -> CheckRunSpec {
    let short = head.as_str().get(..7).unwrap_or(head.as_str());
    let (status, title, detail) = match state {
        ChangeState::Pending => (
            CheckStatus::Queued,
            "Waiting to be ready",
            "Sheltie schedules a pull request once it is approved, its own checks pass and its footprint is computed.",
        ),
        ChangeState::Ready => (
            CheckStatus::Queued,
            "Ready, waiting for a slot",
            "The scheduler will pick this change up in its next epoch.",
        ),
        ChangeState::Merging => (
            CheckStatus::InProgress,
            "Resolving a conflict",
            "This change overlaps another one; the merge engine is combining them.",
        ),
        ChangeState::Verifying => (
            CheckStatus::InProgress,
            "Verifying",
            "Building and testing the targets this change affects.",
        ),
        ChangeState::Committable => (
            CheckStatus::InProgress,
            "Landing",
            "Verified; waiting for the commit check.",
        ),
        ChangeState::Landed => (
            CheckStatus::Completed(CheckConclusion::Success),
            "Landed",
            "This change is on the target branch.",
        ),
        ChangeState::Returned => (
            CheckStatus::Completed(CheckConclusion::Failure),
            "Needs attention",
            "Sheltie could not land this change automatically.",
        ),
        ChangeState::Closed => (
            CheckStatus::Completed(CheckConclusion::Neutral),
            "No longer tracked",
            "The pull request was closed, or a newer commit replaced this one.",
        ),
    };
    CheckRunSpec {
        status,
        title: title.to_owned(),
        summary: format!("{detail}\n\nCommit `{short}`."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head() -> CommitSha {
        CommitSha::parse("0123456789abcdef0123456789abcdef01234567").unwrap()
    }

    #[test]
    fn pending_is_queued() {
        let spec = spec_for(ChangeState::Pending, &head());
        assert_eq!(spec.status, CheckStatus::Queued);
        assert!(spec.summary.contains("`0123456`"));
    }

    #[test]
    fn only_live_states_are_incomplete() {
        for &state in ChangeState::ALL {
            let completed = matches!(spec_for(state, &head()).status, CheckStatus::Completed(_));
            let expect_completed = matches!(
                state,
                ChangeState::Landed | ChangeState::Closed | ChangeState::Returned
            );
            assert_eq!(completed, expect_completed, "{state}");
        }
    }
}
