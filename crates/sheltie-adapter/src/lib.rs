//! Shared core of Sheltie's build-tool adapters.
//!
//! Adapters run on CI runners, never in the control plane. Each one turns a source tree into a
//! [`GraphSnapshot`] and lets [`sheltie_graph::footprint`]
//! compare the snapshots of a change's base and head. This crate holds what they share:
//!
//! - [`Git`]: reading trees and blobs straight from git, without a checkout;
//! - [`PathPolicy`]: files outside the build graph (ignored, or plain data);
//! - [`Adapter`]: what a build tool provides, and [`Tool`] with [`workspace::Workspace`] for
//!   tools whose graph is read from their manifests ([`Generic`] runs the whole pipeline);
//! - [`history`] and [`cli`]: the merge-history replay behind `roi` and the
//!   `footprint` / `graph` / `roi` command line every adapter binary shares;
//! - [`scan`]: hashing helpers for line-based interface extraction.

use std::collections::BTreeSet;
use std::marker::PhantomData;
use std::path::Path;

use sheltie_core::{DiffHash, FootprintReport, RepoPath, TargetId};
use sheltie_graph::GraphSnapshot;

pub mod cli;
pub mod git;
pub mod history;
pub mod policy;
pub mod scan;
pub mod workspace;

pub use git::{Git, TreeFiles, Worktree};
pub use policy::{DEFAULT_IGNORE, PathPolicy};
pub use workspace::{Unit, Workspace};

/// Errors from an adapter.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A git command failed.
    #[error("git {args} failed: {stderr}")]
    Git {
        /// The arguments.
        args: String,
        /// What git printed.
        stderr: String,
    },
    /// The build tool (or reading its configuration) failed.
    #[error("{0}")]
    Tool(String),
    /// Invalid configuration.
    #[error("invalid configuration: {0}")]
    Config(String),
    /// Any other I/O problem.
    #[error("{0}")]
    Io(String),
}

/// What a build-tool adapter provides. The CLI ([`cli::run`]) and the merge-history replay
/// ([`history`]) only need this.
pub trait Adapter {
    /// The binary's name, e.g. `sheltie-go`.
    fn name(&self) -> &'static str;
    /// Name and version recorded in every report, e.g. `sheltie-go/0.4.0`.
    fn version(&self) -> &'static str;
    /// Globs for files outside the build that two changes can still conflict on, on top of
    /// [`policy::COMMON_PLAIN`].
    fn plain(&self) -> &'static [&'static str] {
        &[]
    }
    /// The footprint of `head` against its merge base with `base`.
    fn compute(
        &self,
        git: &Git,
        base: &str,
        head: &str,
        policy: &PathPolicy,
        scratch: &Path,
    ) -> Result<FootprintReport, Error>;
    /// The build graph of one revision.
    fn graph(
        &self,
        git: &Git,
        rev: &str,
        policy: &PathPolicy,
        scratch: &Path,
    ) -> Result<GraphSnapshot, Error>;
}

/// A build tool whose graph is read from the files in a tree, without running the tool.
pub trait Tool {
    /// The binary's name, e.g. `sheltie-go`.
    const NAME: &'static str;
    /// Name and version recorded in every report.
    const VERSION: &'static str;
    /// Tool-specific plain files (see [`Adapter::plain`]).
    const PLAIN: &'static [&'static str] = &[];
    /// The units of one tree, their files, dependencies and interfaces.
    fn workspace(git: &Git, files: &TreeFiles) -> Result<Workspace, Error>;
}

/// The [`Adapter`] for a [`Tool`]: reads both trees from git and assembles their snapshots.
pub struct Generic<T: Tool>(PhantomData<T>);

impl<T: Tool> Generic<T> {
    /// The adapter.
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

impl<T: Tool> Default for Generic<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Tool> std::fmt::Debug for Generic<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(T::NAME)
    }
}

impl<T: Tool> Adapter for Generic<T> {
    fn name(&self) -> &'static str {
        T::NAME
    }
    fn version(&self) -> &'static str {
        T::VERSION
    }
    fn plain(&self) -> &'static [&'static str] {
        T::PLAIN
    }
    fn compute(
        &self,
        git: &Git,
        base: &str,
        head: &str,
        policy: &PathPolicy,
        _scratch: &Path,
    ) -> Result<FootprintReport, Error> {
        let head = git.commit(head)?;
        let base = git.merge_base(&git.commit(base)?, &head)?;
        let base_files = git.files(&base)?;
        let head_files = git.files(&head)?;
        let changed: BTreeSet<RepoPath> = git.changed_paths(&base, &head)?.into_iter().collect();
        let b = T::workspace(git, &base_files)?;
        let h = T::workspace(git, &head_files)?;
        let bs = workspace::assemble(&base_files, &b, policy, &changed);
        let hs = workspace::assemble(&head_files, &h, policy, &changed);
        let relevant: BTreeSet<RepoPath> = changed
            .iter()
            .filter(|p| {
                bs.owners.contains_key(*p) || hs.owners.contains_key(*p) || !policy.is_ignored(p)
            })
            .cloned()
            .collect();
        let mut footprint = sheltie_graph::footprint(&bs, &hs, &relevant, &BTreeSet::new());
        add_sequence_targets(git, &base_files, &changed, &mut footprint)?;
        let leak_reasons = if footprint.leaky {
            workspace::leak_reasons(&[(&b, &bs), (&h, &hs)], &footprint.reads, &relevant)
        } else {
            Vec::new()
        };
        report(T::VERSION, git, base, head, footprint, leak_reasons)
    }

    fn graph(
        &self,
        git: &Git,
        rev: &str,
        policy: &PathPolicy,
        _scratch: &Path,
    ) -> Result<GraphSnapshot, Error> {
        let commit = git.commit(rev)?;
        let files = git.files(&commit)?;
        let ws = T::workspace(git, &files)?;
        Ok(workspace::assemble(&files, &ws, policy, &BTreeSet::new()))
    }
}

/// Assembles a report: base tree, diff hash and the adapter's version.
pub fn report(
    version: &str,
    git: &Git,
    base: sheltie_core::CommitSha,
    head: sheltie_core::CommitSha,
    footprint: sheltie_core::Footprint,
    leak_reasons: Vec<String>,
) -> Result<FootprintReport, Error> {
    Ok(FootprintReport {
        adapter: version.to_owned(),
        base_tree: git.tree(&base)?,
        diff_hash: DiffHash::new(format!(
            "sha256:{}",
            scan::sha256(&git.raw_diff(&base, &head)?)
        )),
        base,
        head,
        footprint,
        leak_reasons,
    })
}

/// Adds the `seq:` targets of `[[sequence]]` rules in `.sheltie/merge.toml` at the base (never
/// the pull request's own copy) that a changed path matches: two changes adding numbered
/// files conflict even when their names differ, so the merge engine renumbers them.
pub fn add_sequence_targets(
    git: &Git,
    base_files: &TreeFiles,
    changed: &BTreeSet<RepoPath>,
    footprint: &mut sheltie_core::Footprint,
) -> Result<(), Error> {
    if let Some(oid) = base_files.oid(".sheltie/merge.toml") {
        let rules = String::from_utf8_lossy(&git.blob(oid)?).into_owned();
        for seq in sequence_targets(&rules, changed) {
            footprint.writes.insert(seq.clone());
            footprint.reads.insert(seq);
        }
    }
    Ok(())
}

/// `seq:<first glob>` for every `[[sequence]]` rule in `rules` that a changed path matches.
/// An unreadable rules file yields nothing; the merge engine reports it.
pub fn sequence_targets(rules: &str, changed: &BTreeSet<RepoPath>) -> BTreeSet<TargetId> {
    let Ok(value) = rules.parse::<toml::Table>() else {
        return BTreeSet::new();
    };
    let Some(sequences) = value.get("sequence").and_then(toml::Value::as_array) else {
        return BTreeSet::new();
    };
    let mut targets = BTreeSet::new();
    for seq in sequences.iter().take(100) {
        let globs: Vec<&str> = seq
            .get("paths")
            .and_then(toml::Value::as_array)
            .map(|a| a.iter().filter_map(toml::Value::as_str).collect())
            .unwrap_or_default();
        let Some(first) = globs.first() else { continue };
        let mut set = globset::GlobSetBuilder::new();
        for g in &globs {
            if let Ok(glob) = globset::Glob::new(g) {
                set.add(glob);
            }
        }
        let Ok(set) = set.build() else { continue };
        if changed.iter().any(|p| set.is_match(p.as_str())) {
            targets.insert(TargetId::new(format!("seq:{first}")));
        }
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_rules_give_a_shared_target() {
        let rules = "[[sequence]]\npaths = [\"migrations/*.sql\"]\nkey = \"^(\\\\d+)_\"\n";
        let changed = |p: &str| BTreeSet::from([RepoPath::new(p)]);
        let t = sequence_targets(rules, &changed("migrations/0002_orders.sql"));
        assert_eq!(t, BTreeSet::from([TargetId::new("seq:migrations/*.sql")]));
        assert!(sequence_targets(rules, &changed("src/lib.rs")).is_empty());
        assert!(sequence_targets("not toml [", &changed("migrations/1_x.sql")).is_empty());
    }
}
