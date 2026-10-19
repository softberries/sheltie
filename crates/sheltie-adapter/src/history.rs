//! A branch's merge history with a footprint per landing, for the ROI report.
//!
//! The landings are the first-parent commits of the branch: each merged pull request (merge
//! or squash commit) and each direct push. A landing's change is its diff against its first
//! parent, and it became ready when it landed (its committer time).
//!
//! Its base, the branch it was tested on, is the merge base of its pull request head with the
//! branch: the second parent of a merge commit, or `refs/pull/<n>/head` for a squash commit
//! whose subject ends in `(#<n>)`, when that ref has been fetched. Otherwise it is unknown.

use std::collections::BTreeSet;
use std::path::Path;

use chrono::{DateTime, Utc};
use sheltie_core::{CommitSha, Footprint, RepoPath, TargetId};
use sheltie_graph::predict::{Touches, predict};
use sheltie_scheduler::roi::HistoricalChange;

use crate::{Adapter, Error, Git, PathPolicy};

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
    pub fn describe(self, adapter: &dyn Adapter) -> String {
        match self {
            Footprints::Predicted => {
                "predicted from each landing's changed files against today's build graph (pessimistic: every change is assumed to alter its targets' interfaces)".to_owned()
            }
            Footprints::Exact => format!("computed by {} for every landing, as CI would", adapter.name()),
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
    /// Committer time of the commit its pull request was based on, if known.
    pub based_at: Option<DateTime<Utc>>,
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
        "--format=%H %P%x1f%ct%x1f%s",
        tip.as_str(),
    ])?;
    let mut landings = Vec::new();
    for line in out.lines() {
        let mut fields = line.splitn(3, '\x1f');
        let (Some(shas), Some(time)) = (fields.next(), fields.next()) else {
            continue;
        };
        let subject = fields.next().unwrap_or_default();
        let mut shas = shas.split(' ');
        let (Some(sha), Some(parent)) = (shas.next(), shas.next().filter(|p| !p.is_empty())) else {
            continue; // a root commit
        };
        let second = shas.next().filter(|p| !p.is_empty());
        let parse = |s: &str| CommitSha::parse(s).map_err(|e| Error::Io(e.to_string()));
        let secs: i64 = time
            .trim()
            .parse()
            .map_err(|_| Error::Io(format!("unexpected commit time {time:?}")))?;
        let at = DateTime::from_timestamp(secs, 0)
            .ok_or_else(|| Error::Io(format!("commit time {secs} out of range")))?;
        let parent = parse(parent)?;
        let head = match second {
            Some(p) => Some(parse(p)?),
            None => {
                pull_number(subject).and_then(|n| git.commit(&format!("refs/pull/{n}/head")).ok())
            }
        };
        let based_at = match head {
            Some(head) => base_time(git, &head, &parent)?,
            None => None,
        };
        landings.push(Landing {
            sha: parse(sha)?,
            parent,
            at,
            based_at,
        });
    }
    landings.reverse();
    Ok(landings)
}

/// The pull request number at the end of a squash commit's subject: `Fix it (#123)`.
fn pull_number(subject: &str) -> Option<u64> {
    let rest = subject.trim_end().strip_suffix(')')?;
    let (_, n) = rest.rsplit_once("(#")?;
    n.parse().ok()
}

/// Committer time of the merge base of a pull request head with the branch before it landed.
fn base_time(
    git: &Git,
    head: &CommitSha,
    parent: &CommitSha,
) -> Result<Option<DateTime<Utc>>, Error> {
    let Ok(base) = git.merge_base(head, parent) else {
        return Ok(None); // unrelated histories, or a head not fetched in full
    };
    let secs: i64 = git
        .run(&["log", "-1", "--format=%ct", base.as_str()])?
        .trim()
        .parse()
        .map_err(|_| Error::Io(format!("unexpected commit time for {base}")))?;
    Ok(DateTime::from_timestamp(secs, 0))
}

/// What computing footprints needs: the adapter, the repository, the path policy and a
/// scratch directory for checkouts.
#[derive(Clone, Copy)]
pub struct Context<'a> {
    /// The build-tool adapter.
    pub adapter: &'a dyn Adapter,
    /// The repository.
    pub git: &'a Git,
    /// Files outside the build.
    pub policy: &'a PathPolicy,
    /// Where temporary checkouts go.
    pub scratch: &'a Path,
}

impl std::fmt::Debug for Context<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("adapter", &self.adapter.name())
            .field("git", self.git)
            .finish_non_exhaustive()
    }
}

/// The footprint of every landing. `progress` is called after each one with (done, total).
pub fn history(
    cx: Context<'_>,
    branch: &str,
    landings: &[Landing],
    mode: Footprints,
    mut progress: impl FnMut(usize, usize),
) -> Result<History, Error> {
    let Context {
        adapter,
        git,
        policy,
        scratch,
    } = cx;
    let snapshot = match mode {
        Footprints::Predicted => Some(adapter.graph(git, branch, policy, scratch)?),
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
            None => adapter
                .compute(git, l.parent.as_str(), l.sha.as_str(), policy, scratch)
                .ok()
                .map(|r| r.footprint),
        };
        match footprint {
            Some(footprint) => changes.push(HistoricalChange {
                ready_at: l.at,
                based_at: l.based_at,
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
