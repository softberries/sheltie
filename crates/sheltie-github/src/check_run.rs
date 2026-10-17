//! What the "Sheltie" check run says for each change state.

use sheltie_core::{ChangeState, CommitSha};
use sheltie_store::{FootprintSummary, ReadinessFlags};

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

/// What a check run is rendered from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckContext<'a> {
    /// The change's state.
    pub state: ChangeState,
    /// Its head commit.
    pub head: &'a CommitSha,
    /// Its footprint, if uploaded.
    pub footprint: Option<FootprintSummary>,
    /// The last readiness check, if any.
    pub readiness: Option<ReadinessFlags>,
    /// The shadow-mode prediction, for ready changes.
    pub prediction: Option<&'a str>,
    /// Why the change was returned, if it was.
    pub returned_reason: Option<&'a str>,
    /// Whether Sheltie lands changes in this repository (otherwise it only predicts).
    pub merge_mode: bool,
}

/// Renders the check run for a change.
pub fn spec_for(ctx: &CheckContext<'_>) -> CheckRunSpec {
    let short = ctx.head.as_str().get(..7).unwrap_or(ctx.head.as_str());
    let waiting_for = waiting_for(ctx);
    let (status, title, detail): (CheckStatus, String, String) = match ctx.state {
        ChangeState::Pending => (
            CheckStatus::Queued,
            "Waiting to be ready".into(),
            match &waiting_for {
                Some(list) => format!("Waiting for: {list}."),
                None => "Checking approval and checks on GitHub.".into(),
            },
        ),
        ChangeState::Ready => (
            CheckStatus::Queued,
            match (ctx.prediction, ctx.merge_mode) {
                (Some(p), false) => format!("Shadow: {p}"),
                (Some(p), true) => format!("Queued: {}", p.replacen("would ", "will ", 1)),
                (None, _) => "Ready, waiting for the next epoch".into(),
            },
            if ctx.merge_mode {
                "Sheltie will land this change; no need to merge it by hand.".into()
            } else {
                "Shadow mode: Sheltie records what it would do with this change but does not merge it.".into()
            },
        ),
        ChangeState::Merging => (
            CheckStatus::InProgress,
            "Resolving a conflict".into(),
            "This change overlaps another one; the merge engine is combining them.".into(),
        ),
        ChangeState::Verifying => (
            CheckStatus::InProgress,
            "Verifying on the current head".into(),
            "The change is merged onto the default branch on a `sheltie/verify/` branch; it lands when the repository's CI passes there.".into(),
        ),
        ChangeState::Committable => (
            CheckStatus::InProgress,
            "Landing".into(),
            "Verified; waiting for the commit check.".into(),
        ),
        ChangeState::Landed => (
            CheckStatus::Completed(CheckConclusion::Success),
            "Landed".into(),
            "This change is on the target branch.".into(),
        ),
        ChangeState::Returned => (
            CheckStatus::Completed(CheckConclusion::Failure),
            "Needs attention".into(),
            match ctx.returned_reason {
                Some(reason) => format!("Sheltie could not land this change: {reason}"),
                None => "Sheltie could not land this change automatically.".into(),
            },
        ),
        ChangeState::Closed => (
            CheckStatus::Completed(CheckConclusion::Neutral),
            "No longer tracked".into(),
            "The pull request was closed, or a newer commit replaced this one.".into(),
        ),
    };
    let footprint = match ctx.footprint {
        None => "Footprint: not computed yet.".to_owned(),
        Some(f) => format!(
            "Footprint: writes {} target{}, reads {}, runs {} test group{}{}.",
            f.writes,
            if f.writes == 1 { "" } else { "s" },
            f.reads,
            f.tests,
            if f.tests == 1 { "" } else { "s" },
            if f.leaky {
                "; **leaky**, widened to everything it may affect"
            } else {
                ""
            },
        ),
    };
    CheckRunSpec {
        status,
        title,
        summary: format!("{detail}\n\n{footprint}\n\nCommit `{short}`."),
    }
}

/// What a pending change still lacks, or `None` if readiness was not checked yet.
fn waiting_for(ctx: &CheckContext<'_>) -> Option<String> {
    let r = ctx.readiness?;
    let mut missing = Vec::new();
    if r.draft {
        missing.push("the pull request to leave draft");
    }
    if !r.approved {
        missing.push("approval");
    }
    if !r.checks_green {
        missing.push("other checks to pass");
    }
    if ctx.footprint.is_none() {
        missing.push("a footprint from CI");
    }
    if missing.is_empty() {
        missing.push("the target branch to match the default branch");
    }
    Some(missing.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head() -> CommitSha {
        CommitSha::parse("0123456789abcdef0123456789abcdef01234567").unwrap()
    }

    fn ctx(state: ChangeState, h: &CommitSha) -> CheckContext<'_> {
        CheckContext {
            state,
            head: h,
            footprint: None,
            readiness: None,
            prediction: None,
            returned_reason: None,
            merge_mode: false,
        }
    }

    #[test]
    fn pending_is_queued() {
        let h = head();
        let spec = spec_for(&ctx(ChangeState::Pending, &h));
        assert_eq!(spec.status, CheckStatus::Queued);
        assert!(spec.summary.contains("`0123456`"));
        assert!(spec.summary.contains("not computed yet"));
    }

    #[test]
    fn pending_lists_what_is_missing() {
        let h = head();
        let mut c = ctx(ChangeState::Pending, &h);
        c.readiness = Some(ReadinessFlags {
            approved: false,
            checks_green: true,
            draft: false,
        });
        let spec = spec_for(&c);
        assert!(
            spec.summary
                .contains("Waiting for: approval, a footprint from CI."),
            "{}",
            spec.summary
        );
    }

    #[test]
    fn ready_shows_the_prediction() {
        let h = head();
        let mut c = ctx(ChangeState::Ready, &h);
        c.prediction = Some("would land now with #12");
        assert_eq!(spec_for(&c).title, "Shadow: would land now with #12");
    }

    #[test]
    fn merge_mode_says_will() {
        let h = head();
        let mut c = ctx(ChangeState::Ready, &h);
        c.prediction = Some("would land now");
        c.merge_mode = true;
        assert_eq!(spec_for(&c).title, "Queued: will land now");
    }

    #[test]
    fn footprint_is_summarized() {
        let h = head();
        let mut c = ctx(ChangeState::Pending, &h);
        c.footprint = Some(FootprintSummary {
            writes: 1,
            reads: 3,
            tests: 2,
            leaky: true,
        });
        let spec = spec_for(&c);
        assert!(
            spec.summary
                .contains("writes 1 target, reads 3, runs 2 test groups; **leaky**"),
            "{}",
            spec.summary
        );
    }

    #[test]
    fn only_finished_states_are_completed() {
        let h = head();
        for &state in ChangeState::ALL {
            let completed = matches!(spec_for(&ctx(state, &h)).status, CheckStatus::Completed(_));
            let expect_completed = matches!(
                state,
                ChangeState::Landed | ChangeState::Closed | ChangeState::Returned
            );
            assert_eq!(completed, expect_completed, "{state}");
        }
    }
}
