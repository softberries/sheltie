//! Conflict resolutions produced by the merge engine.

use serde::{Deserialize, Serialize};

use crate::ids::{ChangeId, RepoPath};
use crate::score::Score;

text_enum! {
    /// The merge-engine tier that resolved a conflict, cheapest first.
    ///
    /// See "Merge engine" in `docs/design.md`. Variants are ordered by cost, so comparisons
    /// read as "cheaper than".
    #[derive(PartialOrd, Ord)]
    pub enum ResolutionTier {
        /// Tier 0: independent footprints; nothing to resolve.
        Commute => "commute",
        /// Tier 1: structural merge on the AST.
        Structural => "structural",
        /// Tier 2: derived files regenerated.
        Regenerate => "regenerate",
        /// Tier 3: merge semantics declared in `.sheltie/merge.toml`.
        Declared => "declared",
        /// Tier 4a: intent replay on the conflicting hunks.
        ReplayHunks => "replay_hunks",
        /// Tier 4b: intent replay of the whole change on the new base.
        ReplayFull => "replay_full",
        /// Tier 5: resolved by a person.
        Human => "human",
    }
}

impl ResolutionTier {
    /// The tier label used in the design doc and in check runs: `0`–`3`, `4a`, `4b`, `5`.
    pub const fn label(self) -> &'static str {
        match self {
            ResolutionTier::Commute => "0",
            ResolutionTier::Structural => "1",
            ResolutionTier::Regenerate => "2",
            ResolutionTier::Declared => "3",
            ResolutionTier::ReplayHunks => "4a",
            ResolutionTier::ReplayFull => "4b",
            ResolutionTier::Human => "5",
        }
    }

    /// True for the tiers that re-derive code with an agent.
    pub const fn is_replay(self) -> bool {
        matches!(
            self,
            ResolutionTier::ReplayHunks | ResolutionTier::ReplayFull
        )
    }
}

/// A recorded resolution: which changes were combined, how, and in which files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resolution {
    /// The changes that were combined, in application order.
    pub changes: Vec<ChangeId>,
    /// The tier that resolved them.
    pub tier: ResolutionTier,
    /// Files the resolution touched.
    pub files: Vec<RepoPath>,
    /// Similarity of a replayed diff to the original; set only for replay tiers.
    pub similarity: Option<Score>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_are_ordered_by_cost() {
        let mut sorted = ResolutionTier::ALL.to_vec();
        sorted.sort();
        assert_eq!(sorted, ResolutionTier::ALL);
        assert!(ResolutionTier::Structural < ResolutionTier::ReplayHunks);
    }

    #[test]
    fn labels_match_the_design_doc() {
        let labels: Vec<_> = ResolutionTier::ALL.iter().map(|t| t.label()).collect();
        assert_eq!(labels, ["0", "1", "2", "3", "4a", "4b", "5"]);
    }

    #[test]
    fn text_form_round_trips() {
        for &t in ResolutionTier::ALL {
            assert_eq!(t.as_str().parse::<ResolutionTier>(), Ok(t));
        }
    }
}
