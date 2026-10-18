//! Conflict graph, epochs and execution strategies.
//!
//! Each epoch, [`plan_epoch`] takes the ready changes with their footprints, plus what has
//! landed on the target branch since each change's base, and decides what should happen to
//! each change: land now, reverify some targets, go to the merge engine, or be built together
//! with its conflicting neighbours. The function is pure; the server runs it on a timer and,
//! in shadow mode (M3), only records and displays the decisions.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;

use serde::Serialize;
use sheltie_core::{Footprint, Relation, TargetId};

pub mod capacity;
pub mod report;
pub mod savings;

/// Largest group built as a speculative chain; bigger ones are group-tested batches.
pub const MAX_CHAIN: usize = 8;

/// A ready change, as the scheduler sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate<K> {
    /// Identifies the change; any ordered key.
    pub key: K,
    /// Base priority; higher lands first.
    pub priority: i64,
    /// Minutes since the change became ready; added to the priority so nothing starves.
    pub waited_minutes: i64,
    /// The change's footprint against its base.
    pub footprint: Footprint,
    /// What landed on the target branch since the change's base: the union of those
    /// changes' write sets, or `None` when some of them have no known footprint.
    pub landed_writes: Option<BTreeSet<TargetId>>,
}

impl<K> Candidate<K> {
    fn effective_priority(&self) -> i64 {
        self.priority.saturating_add(self.waited_minutes)
    }
}

/// What should happen to one change this epoch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision<K> {
    /// Commutes with everything else: commit now. `batch` lists every change landing now,
    /// this one included; they are pairwise independent.
    LandNow {
        /// All changes landing in this epoch.
        batch: Vec<K>,
    },
    /// Something that landed since its base touches its read set: rebuild these targets
    /// on the new head, then commit.
    Reverify {
        /// Targets to rebuild; empty means "unknown, rebuild the whole footprint".
        targets: BTreeSet<TargetId>,
    },
    /// Writes a target that another change writes: the merge engine must combine them.
    MergeEngine {
        /// Ready changes it conflicts with textually.
        with: Vec<K>,
        /// Whether a change that already landed writes the same targets.
        landed: bool,
    },
    /// Part of a small group with semantic conflicts: build the group's prefixes in
    /// priority order and land the longest green one.
    SpeculativeChain {
        /// The group in build order.
        chain: Vec<K>,
        /// This change's position in `chain` (0-based).
        position: usize,
    },
    /// Part of a large group with semantic conflicts: build the whole group once, bisect
    /// on failure.
    GroupBatch {
        /// The group, in priority order.
        group: Vec<K>,
    },
}

impl<K> Decision<K> {
    /// A stable label for storage and metrics.
    pub fn label(&self) -> &'static str {
        match self {
            Decision::LandNow { .. } => "land_now",
            Decision::Reverify { .. } => "reverify",
            Decision::MergeEngine { .. } => "merge_engine",
            Decision::SpeculativeChain { .. } => "speculative_chain",
            Decision::GroupBatch { .. } => "group_batch",
        }
    }
}

impl<K: Clone> Decision<K> {
    /// The same decision with every key mapped through `f` (e.g. database ids to PR numbers).
    pub fn map_keys<J>(&self, f: impl Fn(&K) -> J) -> Decision<J> {
        let map = |v: &Vec<K>| v.iter().map(&f).collect::<Vec<J>>();
        match self {
            Decision::LandNow { batch } => Decision::LandNow { batch: map(batch) },
            Decision::Reverify { targets } => Decision::Reverify {
                targets: targets.clone(),
            },
            Decision::MergeEngine { with, landed } => Decision::MergeEngine {
                with: map(with),
                landed: *landed,
            },
            Decision::SpeculativeChain { chain, position } => Decision::SpeculativeChain {
                chain: map(chain),
                position: *position,
            },
            Decision::GroupBatch { group } => Decision::GroupBatch { group: map(group) },
        }
    }
}

impl<K: PartialEq + std::fmt::Display> Decision<K> {
    /// One line describing the decision for change `me`, for a check run.
    pub fn summary(&self, me: &K) -> String {
        let others = |v: &[K]| {
            v.iter()
                .filter(|k| *k != me)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };
        match self {
            Decision::LandNow { batch } => {
                let with = others(batch);
                if with.is_empty() {
                    "would land now".to_owned()
                } else {
                    format!("would land now, together with {with}")
                }
            }
            Decision::Reverify { targets } if targets.is_empty() => {
                "would reverify first: what landed since its base is not fully known".to_owned()
            }
            Decision::Reverify { targets } => format!(
                "would reverify {} target{} touched by changes landed since its base",
                targets.len(),
                if targets.len() == 1 { "" } else { "s" }
            ),
            Decision::MergeEngine { landed: true, .. } => {
                "writes what already landed since its base; would go to the merge engine".to_owned()
            }
            Decision::MergeEngine { with, .. } => {
                format!(
                    "writes the same targets as {}; would go to the merge engine",
                    others(with)
                )
            }
            Decision::SpeculativeChain { chain, position } => format!(
                "would be built in a speculative chain, {} of {} ({})",
                position + 1,
                chain.len(),
                chain
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" → ")
            ),
            Decision::GroupBatch { group } => {
                format!(
                    "would be built in a batch of {} conflicting changes",
                    group.len()
                )
            }
        }
    }
}

/// Every decision label, for storage constraints.
pub const DECISION_LABELS: [&str; 5] = [
    "land_now",
    "reverify",
    "merge_engine",
    "speculative_chain",
    "group_batch",
];

/// Plans one epoch. Returns exactly one decision per candidate.
///
/// 1. A change that writes something landed since its base goes to the merge engine.
/// 2. Among the rest, changes that share a written target with another ready change go to
///    the merge engine (textual conflicts).
/// 3. The remaining changes form a conflict graph of semantic edges. A change with no edges
///    lands now if nothing landed since its base touches its read set, and is reverified
///    otherwise. A connected group is a speculative chain (up to [`MAX_CHAIN`] changes) or
///    a group-tested batch, ordered by effective priority.
pub fn plan_epoch<K: Ord + Clone + Debug>(candidates: &[Candidate<K>]) -> BTreeMap<K, Decision<K>> {
    let mut decisions = BTreeMap::new();
    let by_key: BTreeMap<&K, &Candidate<K>> = candidates.iter().map(|c| (&c.key, c)).collect();

    // 1. Write conflicts with what already landed.
    let mut remaining: Vec<&Candidate<K>> = Vec::new();
    for c in by_key.values() {
        let landed_write = c
            .landed_writes
            .as_ref()
            .is_some_and(|landed| !landed.is_disjoint(&c.footprint.writes));
        if landed_write {
            decisions.insert(
                c.key.clone(),
                Decision::MergeEngine {
                    with: Vec::new(),
                    landed: true,
                },
            );
        } else {
            remaining.push(c);
        }
    }

    // 2. Textual conflicts among ready changes.
    let mut textual: BTreeMap<K, Vec<K>> = BTreeMap::new();
    for (i, a) in remaining.iter().enumerate() {
        for b in &remaining[i + 1..] {
            if a.footprint.relation(&b.footprint) == Relation::Textual {
                textual
                    .entry(a.key.clone())
                    .or_default()
                    .push(b.key.clone());
                textual
                    .entry(b.key.clone())
                    .or_default()
                    .push(a.key.clone());
            }
        }
    }
    for (key, mut with) in textual.clone() {
        with.sort();
        decisions.insert(
            key,
            Decision::MergeEngine {
                with,
                landed: false,
            },
        );
    }
    remaining.retain(|c| !textual.contains_key(&c.key));

    // 3. Semantic components among the rest.
    let mut land_now = Vec::new();
    for component in components(&remaining) {
        if let [single] = component.as_slice() {
            let touched = single.landed_writes.as_ref().map(|landed| {
                landed
                    .intersection(&single.footprint.reads)
                    .cloned()
                    .collect::<BTreeSet<_>>()
            });
            match touched {
                Some(t) if t.is_empty() => land_now.push(single.key.clone()),
                Some(targets) => {
                    decisions.insert(single.key.clone(), Decision::Reverify { targets });
                }
                None => {
                    decisions.insert(
                        single.key.clone(),
                        Decision::Reverify {
                            targets: BTreeSet::new(),
                        },
                    );
                }
            }
            continue;
        }
        let mut ordered = component;
        ordered.sort_by(|a, b| {
            b.effective_priority()
                .cmp(&a.effective_priority())
                .then_with(|| a.key.cmp(&b.key))
        });
        let keys: Vec<K> = ordered.iter().map(|c| c.key.clone()).collect();
        for (position, c) in ordered.iter().enumerate() {
            let decision = if keys.len() <= MAX_CHAIN {
                Decision::SpeculativeChain {
                    chain: keys.clone(),
                    position,
                }
            } else {
                Decision::GroupBatch {
                    group: keys.clone(),
                }
            };
            decisions.insert(c.key.clone(), decision);
        }
    }
    land_now.sort();
    for key in &land_now {
        decisions.insert(
            key.clone(),
            Decision::LandNow {
                batch: land_now.clone(),
            },
        );
    }
    decisions
}

/// Connected components of the semantic-conflict graph, each sorted by key.
fn components<'a, K: Ord + Clone>(changes: &[&'a Candidate<K>]) -> Vec<Vec<&'a Candidate<K>>> {
    let n = changes.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for i in 0..n {
        for j in i + 1..n {
            if changes[i].footprint.relation(&changes[j].footprint) != Relation::Independent {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                parent[a.max(b)] = a.min(b);
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<&'a Candidate<K>>> = BTreeMap::new();
    for (i, change) in changes.iter().enumerate() {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(change);
    }
    groups
        .into_values()
        .map(|mut g| {
            g.sort_by(|a, b| a.key.cmp(&b.key));
            g
        })
        .collect()
}

#[cfg(test)]
mod tests;
