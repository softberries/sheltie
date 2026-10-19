//! Conflict resolutions produced by the merge engine.

use serde::{Deserialize, Serialize};

use crate::ids::{ChangeId, RepoPath};
use crate::score::Score;

text_enum! {
    /// The merge-engine tier that resolved a conflict, cheapest first.
    ///
    /// Variants are ordered by cost, so comparisons read as "cheaper than".
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
        /// Tier 4a: an agent re-derives the conflicting hunks.
        ReplayHunks => "replay_hunks",
        /// Tier 4b: an agent re-derives the whole change on the new base.
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

/// How one file of a merge was resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileResolution {
    /// The file.
    pub path: RepoPath,
    /// The tier that resolved it.
    pub tier: ResolutionTier,
}

/// What the merge engine reports after merging a pull request onto the head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeReport {
    /// The pull request.
    pub pr: crate::PrNumber,
    /// Its head commit.
    pub head: crate::CommitSha,
    /// The default-branch commit it was merged onto.
    pub onto: crate::CommitSha,
    /// The merge commit (parents: `onto`, `head`), if everything was resolved.
    pub result: Option<crate::CommitSha>,
    /// Files that needed more than git's own merge, and how they were resolved.
    pub files: Vec<FileResolution>,
    /// Files that could not be resolved.
    pub unresolved: Vec<RepoPath>,
    /// The workflow failed before it could merge; then there is no result and nothing else.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub failed: bool,
}

impl MergeReport {
    /// The highest tier used, or [`ResolutionTier::Commute`] when git merged everything.
    pub fn tier(&self) -> ResolutionTier {
        self.files
            .iter()
            .map(|f| f.tier)
            .max()
            .unwrap_or(ResolutionTier::Commute)
    }

    /// Bounds sizes; uploads are untrusted.
    pub fn validate(&self) -> Result<(), crate::InvalidReport> {
        let ok = |p: &RepoPath| {
            !p.as_str().is_empty()
                && p.as_str().len() <= 1024
                && !p.as_str().chars().any(char::is_control)
        };
        if self.files.len() + self.unresolved.len() > 10_000 {
            return Err(crate::InvalidReport("too many files".into()));
        }
        if !self
            .files
            .iter()
            .map(|f| &f.path)
            .chain(&self.unresolved)
            .all(ok)
        {
            return Err(crate::InvalidReport("invalid path".into()));
        }
        if self.failed {
            if self.result.is_some() || !self.files.is_empty() || !self.unresolved.is_empty() {
                return Err(crate::InvalidReport(
                    "a failed run reports no result and no files".into(),
                ));
            }
            return Ok(());
        }
        if self.result.is_some() == !self.unresolved.is_empty() {
            return Err(crate::InvalidReport(
                "a result needs no unresolved files, and vice versa".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_merge_reports() {
        let report = |extra: &str| -> MergeReport {
            serde_json::from_str(&format!(
                r#"{{"pr": 2, "head": "{h}", "onto": "{o}", "result": null, "files": [], "unresolved": []{extra}}}"#,
                h = "a".repeat(40),
                o = "b".repeat(40),
            ))
            .unwrap()
        };
        assert!(report("").validate().is_err(), "neither a result nor files");
        let failed = report(r#", "failed": true"#);
        assert!(failed.failed);
        assert!(failed.validate().is_ok());
        let mut with_files = failed.clone();
        with_files.unresolved = vec![RepoPath::new("src/lib.rs")];
        assert!(with_files.validate().is_err());
        assert!(!report("").failed, "defaults to false");
        assert!(
            !serde_json::to_string(&report(""))
                .unwrap()
                .contains("failed")
        );
    }

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
