//! Changes and their lifecycle.

use serde::{Deserialize, Serialize};

use crate::ids::{ChangeId, CommitSha, DiffHash, IntentRef};
use crate::score::Score;

text_enum! {
    /// Where a change is in Sheltie's pipeline.
    ///
    /// Allowed moves are given by [`ChangeState::can_transition_to`]. `Landed` and `Closed`
    /// are terminal.
    pub enum ChangeState {
        /// Open, but not yet ready: missing approval, green presubmit or footprint.
        Pending => "pending",
        /// Approved, presubmit green on its own base, footprint computed.
        Ready => "ready",
        /// In the merge engine, resolving a textual conflict.
        Merging => "merging",
        /// Verification of its read set is running.
        Verifying => "verifying",
        /// Verified; waiting for the committer's OCC check and CAS on head.
        Committable => "committable",
        /// On the target branch.
        Landed => "landed",
        /// Sent back: a write conflict at commit time, or verification failed.
        Returned => "returned",
        /// Closed without landing, or superseded by a new head.
        Closed => "closed",
    }
}

impl ChangeState {
    /// True for states a change never leaves.
    pub const fn is_terminal(self) -> bool {
        matches!(self, ChangeState::Landed | ChangeState::Closed)
    }

    /// Whether the scheduler may move a change from `self` to `next`.
    ///
    /// Any non-terminal state may move to `Closed`. Staying in the same state is not a
    /// transition.
    pub const fn can_transition_to(self, next: ChangeState) -> bool {
        use ChangeState::*;
        if self.is_terminal() {
            return false;
        }
        matches!(
            (self, next),
            (_, Closed)
                | (Pending, Ready)
                | (Ready, Pending | Merging | Verifying)
                | (Merging, Verifying | Returned)
                | (Verifying, Committable | Returned)
                // A CAS that finds the read set touched reverifies the intersected targets.
                | (Committable, Landed | Verifying | Returned)
                // Returned changes go back to the merge engine or wait for the author.
                | (Returned, Merging | Pending)
        )
    }
}

/// One pull request at one head commit, as the scheduler sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    /// Repository, PR number and head commit.
    pub id: ChangeId,
    /// The commit the PR is based on.
    pub base: CommitSha,
    /// Hash of the PR's diff against `base`.
    pub diff_hash: DiffHash,
    /// The agent's recorded intent, if the PR carries one.
    pub intent: Option<IntentRef>,
    /// Lifecycle state.
    pub state: ChangeState,
    /// Learned risk that landing this change breaks the target branch.
    pub risk: Score,
    /// Scheduling priority; higher lands first. Raised with age for fairness.
    pub priority: i64,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use ChangeState::*;

    #[test]
    fn happy_path_is_allowed() {
        let path = [Pending, Ready, Verifying, Committable, Landed];
        for pair in path.windows(2) {
            assert!(
                pair[0].can_transition_to(pair[1]),
                "{} -> {}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn conflict_path_is_allowed() {
        let path = [Ready, Merging, Verifying, Committable, Returned, Merging];
        for pair in path.windows(2) {
            assert!(
                pair[0].can_transition_to(pair[1]),
                "{} -> {}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn terminal_states_have_no_successors() {
        for from in [Landed, Closed] {
            for &to in ChangeState::ALL {
                assert!(!from.can_transition_to(to), "{from} -> {to}");
            }
        }
    }

    #[test]
    fn every_live_state_can_close() {
        for &s in ChangeState::ALL.iter().filter(|s| !s.is_terminal()) {
            assert!(s.can_transition_to(Closed), "{s} cannot close");
        }
    }

    #[test]
    fn no_self_transitions() {
        for &s in ChangeState::ALL {
            assert!(!s.can_transition_to(s), "{s} -> {s}");
        }
    }

    #[test]
    fn only_committable_lands() {
        for &s in ChangeState::ALL {
            assert_eq!(
                s.can_transition_to(Landed),
                s == Committable,
                "{s} -> landed"
            );
        }
    }

    #[test]
    fn every_state_is_reachable_from_pending() {
        let mut seen = BTreeSet::from([Pending.as_str()]);
        let mut frontier = vec![Pending];
        while let Some(s) = frontier.pop() {
            for &next in ChangeState::ALL {
                if s.can_transition_to(next) && seen.insert(next.as_str()) {
                    frontier.push(next);
                }
            }
        }
        assert_eq!(seen.len(), ChangeState::ALL.len());
    }

    #[test]
    fn text_form_round_trips() {
        for &s in ChangeState::ALL {
            assert_eq!(s.as_str().parse::<ChangeState>(), Ok(s));
            assert_eq!(serde_json::to_string(&s).unwrap(), format!("\"{s}\""));
        }
        assert!("Landed".parse::<ChangeState>().is_err());
    }
}
