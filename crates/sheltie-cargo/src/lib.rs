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
use sheltie_core::{RepoPath, TargetId};

mod lockfile;
mod metadata;
mod modules;
mod snapshot;

use sheltie_adapter::git;
pub use sheltie_adapter::{DEFAULT_IGNORE, Error, Git, PathPolicy, history};

/// Rust tooling files outside the build: tracked (two changes conflict), but not leaky.
pub const CARGO_PLAIN: &[&str] = &[
    "rustfmt.toml",
    ".rustfmt.toml",
    "clippy.toml",
    ".clippy.toml",
    "deny.toml",
];

/// [`CARGO_PLAIN`] and the plain files every adapter shares.
pub const DEFAULT_PLAIN: &[&str] = &[
    ".github/**",
    ".gitlab-ci.yml",
    ".circleci/**",
    ".pre-commit-config.yaml",
    "rustfmt.toml",
    ".rustfmt.toml",
    "clippy.toml",
    ".clippy.toml",
    "deny.toml",
];

/// The Cargo adapter, for the shared command line and history replay.
#[derive(Debug, Clone, Copy, Default)]
pub struct Cargo;

impl sheltie_adapter::Adapter for Cargo {
    fn name(&self) -> &'static str {
        "sheltie-cargo"
    }
    fn version(&self) -> &'static str {
        ADAPTER_VERSION
    }
    fn plain(&self) -> &'static [&'static str] {
        CARGO_PLAIN
    }
    fn compute(
        &self,
        git: &Git,
        base: &str,
        head: &str,
        policy: &PathPolicy,
        scratch: &Path,
    ) -> Result<FootprintReport, Error> {
        compute(git, base, head, policy, scratch)
    }
    fn graph(
        &self,
        git: &Git,
        rev: &str,
        policy: &PathPolicy,
        scratch: &Path,
    ) -> Result<sheltie_graph::GraphSnapshot, Error> {
        graph(git, rev, policy, scratch)
    }
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
    sheltie_adapter::add_sequence_targets(git, &base_files, &changed, &mut footprint)?;
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

    sheltie_adapter::report(ADAPTER_VERSION, git, base, head, footprint, leak_reasons)
}
