//! Cargo build-graph adapter: computes a change's footprint at module granularity.
//!
//! Runs on CI runners, never in the control plane. It reads both trees straight from git,
//! takes the workspace layout from `cargo metadata --no-deps --offline`, parses sources
//! with `syn` (no build needed) and hands two [`GraphSnapshot`](sheltie_graph::GraphSnapshot)s
//! to [`sheltie_graph::footprint`].
//!
//! Targets are modules (`pkg::billing::tax`), crate roots (`pkg`), other crates of a package
//! (`pkg/bin/name`, `pkg/test/name`), plus `$workspace` (workspace build settings),
//! `$lockfile`, `pkg/files` (other files in a package; not hermetic) and `file:<path>`.

use std::collections::BTreeSet;
use std::path::Path;

pub use sheltie_core::FootprintReport;
use sheltie_core::{DiffHash, RepoPath, TargetId};

mod git;
mod lockfile;
mod metadata;
mod modules;
mod policy;
mod snapshot;

pub use git::Git;
pub use policy::{DEFAULT_IGNORE, DEFAULT_PLAIN, PathPolicy};

/// Errors from the adapter.
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
    /// `cargo metadata` failed.
    #[error("cargo metadata failed: {0}")]
    Metadata(String),
    /// Invalid configuration.
    #[error("invalid configuration: {0}")]
    Config(String),
    /// Any other I/O problem.
    #[error("{0}")]
    Io(String),
}

/// Version of the footprint format and algorithm; part of every report.
pub const ADAPTER_VERSION: &str = concat!("sheltie-cargo/", env!("CARGO_PKG_VERSION"));

/// Builds the graph snapshot of one revision, for inspection and debugging.
pub fn graph(
    git: &Git,
    rev: &str,
    policy: &PathPolicy,
    scratch: &Path,
) -> Result<sheltie_graph::GraphSnapshot, Error> {
    let commit = git.commit(rev)?;
    let files = git.files(&commit)?;
    let layout = {
        let wt = git::Worktree::add(git, &commit, scratch)?;
        metadata::layout(wt.path())?
    };
    Ok(snapshot::build(git, &files, &layout, policy, &BTreeSet::new())?.snapshot)
}

/// `seq:<first glob>` for every `[[sequence]]` rule in `rules` that a changed path matches.
/// An unreadable rules file yields nothing; the merge engine reports it.
fn sequence_targets(rules: &str, changed: &BTreeSet<RepoPath>) -> BTreeSet<TargetId> {
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

/// Computes the footprint of `head` against `base` in the repository `git`.
///
/// `scratch` is where temporary worktrees for `cargo metadata` are created.
pub fn compute(
    git: &Git,
    base: &str,
    head: &str,
    policy: &PathPolicy,
    scratch: &Path,
) -> Result<FootprintReport, Error> {
    let head = git.commit(head)?;
    let base = git.merge_base(&git.commit(base)?, &head)?;
    let base_files = git.files(&base)?;
    let head_files = git.files(&head)?;
    let changed: BTreeSet<RepoPath> = git.changed_paths(&base, &head)?.into_iter().collect();

    let head_layout = {
        let wt = git::Worktree::add(git, &head, scratch)?;
        metadata::layout(wt.path())?
    };
    let manifests_changed = changed
        .iter()
        .any(|p| p.as_str() == "Cargo.toml" || p.as_str().ends_with("/Cargo.toml"));
    let base_layout = if manifests_changed {
        let wt = git::Worktree::add(git, &base, scratch)?;
        metadata::layout(wt.path())?
    } else {
        head_layout.clone()
    };

    let b = snapshot::build(git, &base_files, &base_layout, policy, &changed)?;
    let h = snapshot::build(git, &head_files, &head_layout, policy, &changed)?;

    // Paths nobody owns and the policy ignores do not count.
    let relevant: BTreeSet<RepoPath> = changed
        .iter()
        .filter(|p| {
            b.snapshot.owners.contains_key(*p)
                || h.snapshot.owners.contains_key(*p)
                || !policy.is_ignored(p)
        })
        .cloned()
        .collect();

    let mut extra_reads = BTreeSet::new();
    let mut leak_reasons = Vec::new();
    if changed.contains(&RepoPath::new("Cargo.lock")) {
        let read = |files: &git::TreeFiles| -> Result<Option<String>, Error> {
            files
                .oid("Cargo.lock")
                .map(|oid| {
                    git.blob(oid)
                        .map(|b| String::from_utf8_lossy(&b).into_owned())
                })
                .transpose()
        };
        match lockfile::affected_packages(
            read(&base_files)?.as_deref(),
            read(&head_files)?.as_deref(),
        ) {
            Some(pkgs) => {
                for pkg in pkgs {
                    for built in [&b, &h] {
                        extra_reads.extend(
                            built
                                .package_targets
                                .get(&pkg)
                                .into_iter()
                                .flatten()
                                .cloned(),
                        );
                    }
                }
            }
            None => {
                leak_reasons.push("Cargo.lock could not be parsed".into());
                extra_reads.extend(h.snapshot.targets.keys().cloned());
            }
        }
    }

    let mut footprint = sheltie_graph::footprint(&b.snapshot, &h.snapshot, &relevant, &extra_reads);
    // Numbered files (`[[sequence]]` in `.sheltie/merge.toml` at the base, never the PR's) share
    // one target per rule: two changes adding migrations conflict even when their file names
    // differ, so the merge engine renumbers them.
    if let Some(oid) = base_files.oid(".sheltie/merge.toml") {
        let rules = String::from_utf8_lossy(&git.blob(oid)?).into_owned();
        for seq in sequence_targets(&rules, &changed) {
            footprint.writes.insert(seq.clone());
            footprint.reads.insert(seq);
        }
    }
    if !leak_reasons.is_empty() {
        footprint.leaky = true;
    }
    if footprint.leaky {
        let read_packages: BTreeSet<&String> = h
            .package_targets
            .iter()
            .chain(&b.package_targets)
            .filter(|(_, ts)| ts.iter().any(|t| footprint.reads.contains(t)))
            .map(|(p, _)| p)
            .collect();
        for built in [&b, &h] {
            for (pkg, reasons) in &built.leaks {
                if read_packages.contains(pkg) {
                    leak_reasons.extend(reasons.iter().map(|r| format!("{pkg}: {r}")));
                }
            }
        }
        for p in &relevant {
            let owned =
                |s: &sheltie_graph::GraphSnapshot| s.owners.get(p).cloned().unwrap_or_default();
            let owners: BTreeSet<TargetId> = owned(&b.snapshot)
                .into_iter()
                .chain(owned(&h.snapshot))
                .collect();
            if owners.iter().any(|o| {
                h.snapshot
                    .targets
                    .get(o)
                    .or_else(|| b.snapshot.targets.get(o))
                    .is_some_and(|t| !t.hermetic)
                    && (o.as_str().starts_with("file:") || o.as_str().ends_with("/files"))
            }) {
                leak_reasons.push(format!("{p} is outside the module graph"));
            }
        }
        leak_reasons.sort();
        leak_reasons.dedup();
    }

    Ok(FootprintReport {
        adapter: ADAPTER_VERSION.to_owned(),
        base_tree: git.tree(&base)?,
        diff_hash: DiffHash::new(format!(
            "sha256:{}",
            snapshot::sha256(&git.raw_diff(&base, &head)?)
        )),
        base,
        head,
        footprint,
        leak_reasons,
    })
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
