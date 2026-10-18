//! A branch's merge history with a footprint per landing, for the ROI report.
//!
//! The landings are the first-parent commits of the branch: each merged pull request (merge
//! or squash commit) and each direct push. A landing's change is its diff against its first
//! parent, and it became ready when it landed (its committer time).

use std::collections::BTreeSet;
use std::path::Path;

use chrono::{DateTime, Utc};
use sheltie_core::{CommitSha, Footprint, RepoPath, TargetId};
use sheltie_graph::predict::{Touches, predict};
use sheltie_scheduler::roi::HistoricalChange;

use crate::{Error, Git, PathPolicy};

/// How each landing's footprint is computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Footprints {
    /// From the changed files, against one build graph of the branch tip. Fast; pessimistic
    /// (every written target is assumed to change its interface).
    Predicted,
    /// The full adapter on every landing, as CI would. Slower.
    Exact,
}

impl Footprints {
    /// How the report describes it.
    pub fn describe(self) -> &'static str {
        match self {
            Footprints::Predicted => {
                "predicted from each landing's changed files against today's build graph (pessimistic: every change is assumed to alter its targets' interfaces)"
            }
            Footprints::Exact => "computed by sheltie-cargo for every landing, as CI would",
        }
    }
}

/// One first-parent commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landing {
    /// The commit.
    pub sha: CommitSha,
    /// Its first parent.
    pub parent: CommitSha,
    /// Committer time.
    pub at: DateTime<Utc>,
}

/// The history, ready to replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct History {
    /// Landings with a footprint, oldest first.
    pub changes: Vec<HistoricalChange>,
    /// Landings whose footprint could not be computed.
    pub skipped: usize,
}

/// First-parent commits of `branch` committed in the last `days` days, oldest first. Root
/// commits are left out.
pub fn landings(git: &Git, branch: &str, days: u32) -> Result<Vec<Landing>, Error> {
    let tip = git.commit(branch)?;
    let since = format!("--since={days}.days.ago");
    let out = git.run(&[
        "log",
        "--first-parent",
        &since,
        "--format=%H %P%x1f%ct",
        tip.as_str(),
    ])?;
    let mut landings = Vec::new();
    for line in out.lines() {
        let Some((shas, time)) = line.split_once('\x1f') else {
            continue;
        };
        let mut shas = shas.split(' ');
        let (Some(sha), Some(parent)) = (shas.next(), shas.next().filter(|p| !p.is_empty())) else {
            continue; // a root commit
        };
        let parse = |s: &str| CommitSha::parse(s).map_err(|e| Error::Io(e.to_string()));
        let secs: i64 = time
            .trim()
            .parse()
            .map_err(|_| Error::Io(format!("unexpected commit time {time:?}")))?;
        let at = DateTime::from_timestamp(secs, 0)
            .ok_or_else(|| Error::Io(format!("commit time {secs} out of range")))?;
        landings.push(Landing {
            sha: parse(sha)?,
            parent: parse(parent)?,
            at,
        });
    }
    landings.reverse();
    Ok(landings)
}

/// The footprint of every landing. `progress` is called after each one with (done, total).
pub fn history(
    git: &Git,
    branch: &str,
    landings: &[Landing],
    mode: Footprints,
    policy: &PathPolicy,
    scratch: &Path,
    mut progress: impl FnMut(usize, usize),
) -> Result<History, Error> {
    let snapshot = match mode {
        Footprints::Predicted => Some(crate::graph(git, branch, policy, scratch)?),
        Footprints::Exact => None,
    };
    let mut changes = Vec::with_capacity(landings.len());
    let mut skipped = 0;
    for (i, l) in landings.iter().enumerate() {
        let footprint = match &snapshot {
            Some(snapshot) => {
                let paths: BTreeSet<RepoPath> = git
                    .changed_paths(&l.parent, &l.sha)?
                    .into_iter()
                    .filter(|p| !policy.is_ignored(p))
                    .collect();
                Some(predicted(snapshot, paths))
            }
            None => crate::compute(git, l.parent.as_str(), l.sha.as_str(), policy, scratch)
                .ok()
                .map(|r| r.footprint),
        };
        match footprint {
            Some(footprint) => changes.push(HistoricalChange {
                ready_at: l.at,
                footprint,
            }),
            None => skipped += 1,
        }
        progress(i + 1, landings.len());
    }
    Ok(History { changes, skipped })
}

/// A predicted footprint; files no target owns count as `file:<path>` targets, so two
/// changes to the same such file still conflict.
fn predicted(snapshot: &sheltie_graph::GraphSnapshot, paths: BTreeSet<RepoPath>) -> Footprint {
    let unowned: Vec<TargetId> = paths
        .iter()
        .filter(|p| !snapshot.owners.contains_key(*p))
        .map(|p| TargetId::new(format!("file:{}", p.as_str())))
        .collect();
    let mut fp = predict(
        snapshot,
        &Touches {
            paths,
            targets: BTreeSet::new(),
        },
    )
    .unwrap_or_default();
    fp.writes.extend(unowned.iter().cloned());
    fp.reads.extend(unowned);
    fp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_no_target_owns_are_their_own_targets() {
        let snapshot = sheltie_graph::GraphSnapshot::default();
        let fp = predicted(
            &snapshot,
            [RepoPath::new("scripts/deploy.sh")].into_iter().collect(),
        );
        let expected: BTreeSet<TargetId> = [TargetId::new("file:scripts/deploy.sh")].into();
        assert_eq!((fp.writes, fp.reads), (expected.clone(), expected));
        assert_eq!(predicted(&snapshot, BTreeSet::new()), Footprint::default());
    }
}
