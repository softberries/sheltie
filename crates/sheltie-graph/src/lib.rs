//! Build-graph snapshots and the footprint algorithm.
//!
//! An adapter (such as `sheltie-cargo`) describes one source tree as a [`GraphSnapshot`]:
//! its targets, which files each target owns, which targets read which, interface hashes,
//! covering tests and hermeticity. [`footprint`] compares the snapshots of a change's base and
//! head trees and returns its write and read sets. This crate is pure and deterministic.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sheltie_core::{Footprint, RepoPath, TargetId, TestId};

/// Hash of a target's interface; equal hashes mean readers are unaffected.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IfaceHash(pub String);

/// Which interface of a target an edge depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// Reads the target's own interface (same crate, for Cargo).
    Local,
    /// Reads the target's public surface (another crate, for Cargo).
    Public,
}

/// The taint zone a non-hermetic target widens the read set to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Zone {
    /// A named group of targets, such as a Cargo package.
    Group(String),
    /// Every target in the repository.
    All,
}

/// One target in a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    /// Where a leak in this target can spread.
    pub zone: Zone,
    /// Interface seen by [`EdgeKind::Local`] readers.
    pub iface: IfaceHash,
    /// Interface seen by [`EdgeKind::Public`] readers.
    pub public_iface: IfaceHash,
    /// Targets this target reads, and through which interface.
    pub reads: BTreeMap<TargetId, EdgeKind>,
    /// Tests that must run when this target is affected.
    pub tests: BTreeSet<TestId>,
    /// Whether the target's build is a pure function of its declared inputs.
    pub hermetic: bool,
}

/// A build graph of one source tree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphSnapshot {
    /// All targets.
    pub targets: BTreeMap<TargetId, Target>,
    /// For each file, the targets it belongs to.
    pub owners: BTreeMap<RepoPath, BTreeSet<TargetId>>,
}

/// Computes the footprint of a change from its base and head snapshots.
///
/// - W: the owners of every changed path, in either tree.
/// - R: W, plus readers reached through changed interfaces: a reader of a target whose
///   interface changed is visited, and its own interface is checked in turn. Targets that a
///   written target reads in head but not in base (dependencies the change adds) are in R
///   too, without their tests. `extra_reads`
///   are added as visited (e.g. crates affected by a lockfile change).
/// - Tests: the covering tests of every target in R.
/// - Leaky: some visited target is not hermetic; R then also covers that target's zone and
///   every target that reads into the zone. A changed path that no target owns is treated
///   as leaky across the whole repository.
pub fn footprint(
    base: &GraphSnapshot,
    head: &GraphSnapshot,
    changed: &BTreeSet<RepoPath>,
    extra_reads: &BTreeSet<TargetId>,
) -> Footprint {
    let mut writes = BTreeSet::new();
    let mut unowned = false;
    for path in changed {
        let owners: Vec<&TargetId> = [base, head]
            .iter()
            .filter_map(|s| s.owners.get(path))
            .flatten()
            .collect();
        unowned |= owners.is_empty();
        writes.extend(owners.into_iter().cloned());
    }
    let mut fp = expand(base, head, writes, unowned, extra_reads);
    // A dependency the change adds is something it now reads: a concurrent change to that
    // target's interface conflicts with it, although in the other change's trees nothing
    // reads it yet. (Existing edges are covered from the other side: the other change's
    // read set reaches this one's written targets.)
    let added: BTreeSet<TargetId> = fp
        .writes
        .iter()
        .filter_map(|w| head.targets.get(w).map(|t| (w, t)))
        .flat_map(|(w, t)| {
            let before = base.targets.get(w).map(|b| &b.reads);
            t.reads
                .keys()
                .filter(move |r| before.is_none_or(|b| !b.contains_key(*r)))
                .cloned()
        })
        .collect();
    fp.reads.extend(added);
    fp
}

/// R, tests and leakiness for a write set (see [`footprint`]).
fn expand(
    base: &GraphSnapshot,
    head: &GraphSnapshot,
    writes: BTreeSet<TargetId>,
    unowned: bool,
    extra_reads: &BTreeSet<TargetId>,
) -> Footprint {
    let readers = reverse_edges(base, head);
    let mut reads = writes.clone();
    reads.extend(extra_reads.iter().cloned());
    let mut queue: Vec<TargetId> = reads.iter().cloned().collect();
    while let Some(t) = queue.pop() {
        let (local_changed, public_changed) = iface_changed(base, head, &t);
        if !local_changed && !public_changed {
            continue;
        }
        for (reader, kind) in readers.get(&t).into_iter().flatten() {
            let affected = match kind {
                EdgeKind::Local => local_changed,
                EdgeKind::Public => public_changed,
            };
            if affected && reads.insert(reader.clone()) {
                queue.push(reader.clone());
            }
        }
    }

    let mut leaky = unowned;
    if unowned {
        reads.extend(all_targets(base, head));
    } else {
        let tainted: BTreeSet<Zone> = reads
            .iter()
            .filter_map(|t| target(base, head, t))
            .filter(|t| !t.hermetic)
            .map(|t| t.zone.clone())
            .collect();
        if !tainted.is_empty() {
            leaky = true;
            let zone: BTreeSet<TargetId> = all_targets(base, head)
                .filter(|id| {
                    target(base, head, id)
                        .is_some_and(|t| tainted.contains(&Zone::All) || tainted.contains(&t.zone))
                })
                .collect();
            let into_zone: BTreeSet<TargetId> = zone
                .iter()
                .flat_map(|z| readers.get(z).into_iter().flatten())
                .map(|(reader, _)| reader.clone())
                .collect();
            reads.extend(zone);
            reads.extend(into_zone);
        }
    }

    let tests = reads
        .iter()
        .filter_map(|t| target(base, head, t))
        .flat_map(|t| t.tests.iter().cloned())
        .collect();
    Footprint {
        writes,
        reads,
        tests,
        leaky,
    }
}

/// The target as it is in head, or in base if the change deleted it.
fn target<'a>(
    base: &'a GraphSnapshot,
    head: &'a GraphSnapshot,
    id: &TargetId,
) -> Option<&'a Target> {
    head.targets.get(id).or_else(|| base.targets.get(id))
}

fn all_targets<'a>(
    base: &'a GraphSnapshot,
    head: &'a GraphSnapshot,
) -> impl Iterator<Item = TargetId> + 'a {
    base.targets
        .keys()
        .chain(head.targets.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
}

/// Whether the local and public interfaces differ between the trees. A target that exists
/// in only one tree has changed both.
fn iface_changed(base: &GraphSnapshot, head: &GraphSnapshot, id: &TargetId) -> (bool, bool) {
    match (base.targets.get(id), head.targets.get(id)) {
        (Some(b), Some(h)) => (b.iface != h.iface, b.public_iface != h.public_iface),
        _ => (true, true),
    }
}

/// For each target, who reads it and how, across both trees.
fn reverse_edges(
    base: &GraphSnapshot,
    head: &GraphSnapshot,
) -> BTreeMap<TargetId, BTreeSet<(TargetId, EdgeKind)>> {
    let mut readers: BTreeMap<TargetId, BTreeSet<(TargetId, EdgeKind)>> = BTreeMap::new();
    for snapshot in [base, head] {
        for (reader, t) in &snapshot.targets {
            for (read, kind) in &t.reads {
                readers
                    .entry(read.clone())
                    .or_default()
                    .insert((reader.clone(), *kind));
            }
        }
    }
    readers
}

pub mod predict;

#[cfg(test)]
mod tests;
