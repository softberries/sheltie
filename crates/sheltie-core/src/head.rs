//! Head events: each compare-and-swap that advanced the target branch.

use serde::{Deserialize, Serialize};

use crate::ids::{ChangeId, CommitSha};

text_enum! {
    /// How strongly a commit on the target branch is known to be green.
    pub enum GuaranteeTier {
        /// Landed under the commutativity rule: verified on a base whose later changes
        /// don't touch its read set.
        FootprintProven => "footprint",
        /// Landed after building the exact combined state.
        SpeculationVerified => "speculation",
        /// Covered by a full build of the branch at or after this commit.
        SweepVerified => "sweep",
    }
}

/// One successful compare-and-swap on the target branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadEvent {
    /// CAS sequence number, strictly increasing per repository.
    pub seq: u64,
    /// The new head commit.
    pub commit: CommitSha,
    /// The change that landed.
    pub change: ChangeId,
    /// The guarantee it landed with.
    pub tier: GuaranteeTier,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_form_matches_commit_trailer() {
        // The commit trailer is `Sheltie-Verified: <tier> S=<base>`.
        assert_eq!(GuaranteeTier::FootprintProven.to_string(), "footprint");
        for &t in GuaranteeTier::ALL {
            assert_eq!(t.as_str().parse::<GuaranteeTier>(), Ok(t));
        }
    }
}
