//! Parallelism capacity: how many agents a repository can keep busy at once before their
//! changes start colliding.
//!
//! The model takes the footprints of the repository's recent changes as the distribution an
//! agent draws its next change from. With `N` agents each holding one open change, a change
//! is *clean* when it is independent of the other `N - 1`: it lands without a combined build
//! or the merge engine. For a change `i` that conflicts (textually or semantically) with a
//! share `c_i` of the history, the chance of staying clean is `(1 - c_i)^(N - 1)`; the clean
//! share at `N` agents is the average over the history. The capacity is the largest `N` whose
//! clean share stays at or above a threshold.
//!
//! Draws are assumed independent, so changes that cluster in time (a refactoring touching
//! one crate, then follow-ups) make the real collision rate higher than the model's. See
//! "Parallelism" in `docs/dashboard.md`.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use sheltie_core::{Footprint, TargetId};

/// Fewer footprints than this are too few to estimate from.
pub const MIN_FOOTPRINTS: usize = 20;
/// The clean share the capacity keeps by default: four in five changes land without a
/// combined build or the merge engine.
pub const DEFAULT_CLEAN_SHARE: f64 = 0.8;
/// The largest number of agents considered.
pub const MAX_AGENTS: u32 = 256;
/// Only the most recent changes are used, to bound the pairwise comparison.
pub const MAX_FOOTPRINTS: usize = 4_000;
/// Agent counts shown in the curve.
pub const CURVE: [u32; 7] = [1, 2, 4, 8, 16, 32, 64];
/// Hot targets reported.
pub const HOT_TARGETS: usize = 3;

/// The clean and colliding shares at one number of agents.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CapacityPoint {
    /// Agents with one open change each.
    pub agents: u32,
    /// Expected share of changes independent of all the others.
    pub clean_share: f64,
    /// Expected share writing a target another open change writes (merge engine).
    pub textual_share: f64,
}

/// A target that takes part in many conflicts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HotTarget {
    /// The target.
    pub target: TargetId,
    /// Conflicting pairs of changes it is involved in.
    pub pairs: u64,
}

/// The estimate for one repository.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Capacity {
    /// Footprints it was computed from.
    pub footprints: usize,
    /// Share of pairs of changes that conflict (textually or semantically).
    pub conflicting_pairs: f64,
    /// The clean share the capacity keeps.
    pub threshold: f64,
    /// Largest number of concurrent agents whose clean share stays at or above `threshold`.
    pub agents: u32,
    /// True when even [`MAX_AGENTS`] stay above the threshold: the capacity is at least that.
    pub at_least: bool,
    /// Clean and textual shares at the agent counts in [`CURVE`].
    pub curve: Vec<CapacityPoint>,
    /// Targets involved in the most conflicting pairs, most first.
    pub hot_targets: Vec<HotTarget>,
}

/// Estimates the capacity from recent footprints, most recent last. Returns `None` with
/// fewer than [`MIN_FOOTPRINTS`]. `threshold` is clamped to `0.05..=0.99`.
pub fn capacity(footprints: &[Footprint], threshold: f64) -> Option<Capacity> {
    let recent = &footprints[footprints.len().saturating_sub(MAX_FOOTPRINTS)..];
    let n = recent.len();
    if n < MIN_FOOTPRINTS {
        return None;
    }
    let threshold = if threshold.is_finite() {
        threshold.clamp(0.05, 0.99)
    } else {
        DEFAULT_CLEAN_SHARE
    };

    // Intern target names so the pairwise loop compares sorted integer lists.
    fn intern<'a>(
        names: &mut BTreeMap<&'a TargetId, u32>,
        set: &'a BTreeSet<TargetId>,
    ) -> Vec<u32> {
        let mut ids: Vec<u32> = Vec::with_capacity(set.len());
        for t in set {
            let next = u32::try_from(names.len()).unwrap_or(u32::MAX);
            ids.push(*names.entry(t).or_insert(next));
        }
        ids.sort_unstable();
        ids
    }
    let mut names: BTreeMap<&TargetId, u32> = BTreeMap::new();
    let interned: Vec<(Vec<u32>, Vec<u32>)> = recent
        .iter()
        .map(|f| (intern(&mut names, &f.writes), intern(&mut names, &f.reads)))
        .collect();
    let mut by_id: Vec<Option<&TargetId>> = vec![None; names.len()];
    for (t, &i) in &names {
        if let Some(slot) = by_id.get_mut(i as usize) {
            *slot = Some(*t);
        }
    }

    let mut conflicts = vec![0u32; n];
    let mut textual = vec![0u32; n];
    let mut pairs = 0u64;
    for i in 0..n {
        let (wa, ra) = &interned[i];
        for j in i + 1..n {
            let (wb, rb) = &interned[j];
            // Same rule as `Footprint::relation`: textual first, then semantic.
            let is_textual = intersects(wa, wb);
            if !is_textual && !intersects(wa, rb) && !intersects(wb, ra) {
                continue;
            }
            pairs += 1;
            conflicts[i] += 1;
            conflicts[j] += 1;
            if is_textual {
                textual[i] += 1;
                textual[j] += 1;
            }
        }
    }

    // Pairs a target links: both changes read it (writes are reads too) and at least one
    // writes it, i.e. all pairs of its readers minus the pairs of readers that do not write it.
    let mut readers = vec![0u64; by_id.len()];
    let mut writers = vec![0u64; by_id.len()];
    for (w, r) in &interned {
        w.iter().for_each(|&t| bump(&mut writers, t));
        r.iter().for_each(|&t| bump(&mut readers, t));
    }
    let pairs_of = |k: u64| k * k.saturating_sub(1) / 2;
    let hot: Vec<u64> = readers
        .iter()
        .zip(&writers)
        .map(|(&r, &w)| pairs_of(r) - pairs_of(r.saturating_sub(w)))
        .collect();

    // Shares of the *other* changes each one conflicts with.
    let others = (n - 1) as f64;
    let c: Vec<f64> = conflicts.iter().map(|&k| f64::from(k) / others).collect();
    let t: Vec<f64> = textual.iter().map(|&k| f64::from(k) / others).collect();
    let mean_survival = |shares: &[f64], agents: u32| {
        let exp = i32::try_from(agents.saturating_sub(1)).unwrap_or(i32::MAX);
        shares.iter().map(|s| (1.0 - s).powi(exp)).sum::<f64>() / n as f64
    };
    let point = |agents: u32| CapacityPoint {
        agents,
        clean_share: mean_survival(&c, agents),
        textual_share: 1.0 - mean_survival(&t, agents),
    };

    // The clean share only falls as agents are added, so the first miss ends the search.
    let mut agents = 1;
    for k in 2..=MAX_AGENTS {
        if mean_survival(&c, k) < threshold {
            break;
        }
        agents = k;
    }

    let mut hot_targets: Vec<HotTarget> = hot
        .iter()
        .zip(&by_id)
        .filter(|(pairs, _)| **pairs > 0)
        .filter_map(|(&pairs, t)| {
            t.map(|t| HotTarget {
                target: t.clone(),
                pairs,
            })
        })
        .collect();
    hot_targets.sort_by(|a, b| b.pairs.cmp(&a.pairs).then_with(|| a.target.cmp(&b.target)));
    hot_targets.truncate(HOT_TARGETS);

    let all_pairs = (n as u64) * (n as u64 - 1) / 2;
    Some(Capacity {
        footprints: n,
        conflicting_pairs: pairs as f64 / all_pairs as f64,
        threshold,
        agents,
        at_least: agents == MAX_AGENTS,
        curve: CURVE.iter().map(|&a| point(a)).collect(),
        hot_targets,
    })
}

/// Whether two sorted lists share an element.
fn intersects(a: &[u32], b: &[u32]) -> bool {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => return true,
        }
    }
    false
}

fn bump(counts: &mut [u64], id: u32) {
    if let Some(c) = counts.get_mut(id as usize) {
        *c += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn fp(writes: &[&str], reads: &[&str]) -> Footprint {
        let set = |v: &[&str]| v.iter().map(|s| TargetId::new(*s)).collect::<BTreeSet<_>>();
        let writes = set(writes);
        let mut reads = set(reads);
        reads.extend(writes.iter().cloned());
        Footprint {
            writes,
            reads,
            ..Footprint::default()
        }
    }

    #[test]
    fn too_few_footprints_give_no_estimate() {
        let few = vec![fp(&["a"], &[]); MIN_FOOTPRINTS - 1];
        assert_eq!(capacity(&few, DEFAULT_CLEAN_SHARE), None);
    }

    #[test]
    fn independent_changes_have_no_limit() {
        let all: Vec<Footprint> = (0..30).map(|i| fp(&[&format!("t{i}")], &[])).collect();
        let c = capacity(&all, DEFAULT_CLEAN_SHARE).unwrap();
        assert_eq!(c.conflicting_pairs, 0.0);
        assert!(c.at_least);
        assert_eq!(c.agents, MAX_AGENTS);
        assert!(c.curve.iter().all(|p| p.clean_share == 1.0));
        assert!(c.hot_targets.is_empty());
    }

    #[test]
    fn one_shared_target_caps_the_agents() {
        // A quarter of the changes write `core`; the rest are independent.
        let all: Vec<Footprint> = (0..40)
            .map(|i| {
                if i % 4 == 0 {
                    fp(&["core"], &[])
                } else {
                    fp(&[&format!("t{i}")], &[])
                }
            })
            .collect();
        let c = capacity(&all, DEFAULT_CLEAN_SHARE).unwrap();
        // Each `core` change conflicts with 9 of 39 others; the rest with none.
        let clean = |n: i32| 0.75 + 0.25 * (1.0 - 9.0 / 39.0_f64).powi(n - 1);
        assert!((c.curve[1].clean_share - clean(2)).abs() < 1e-12);
        assert!(clean(c.agents as i32) >= 0.8 && clean(c.agents as i32 + 1) < 0.8);
        assert_eq!(c.agents, 7);
        assert_eq!(
            c.hot_targets,
            vec![HotTarget {
                target: TargetId::new("core"),
                pairs: 45
            }]
        );
        assert!(c.curve[1].textual_share > 0.0);
    }

    #[test]
    fn semantic_conflicts_count_but_are_not_textual() {
        // Half write `api`, half read it.
        let all: Vec<Footprint> = (0..20)
            .map(|i| {
                if i % 2 == 0 {
                    fp(&[&format!("w{i}"), "api"], &[])
                } else {
                    fp(&[&format!("r{i}")], &["api"])
                }
            })
            .collect();
        let c = capacity(&all, DEFAULT_CLEAN_SHARE).unwrap();
        assert!(c.curve[1].clean_share < 1.0);
        assert_eq!(c.hot_targets[0].target, TargetId::new("api"));
    }

    #[test]
    fn a_stricter_threshold_never_allows_more_agents() {
        let all: Vec<Footprint> = (0..30)
            .map(|i| fp(&[&format!("t{}", i % 7)], &[]))
            .collect();
        let loose = capacity(&all, 0.5).unwrap().agents;
        let strict = capacity(&all, 0.95).unwrap().agents;
        assert!(strict <= loose);
    }

    #[test]
    fn only_the_most_recent_footprints_are_used() {
        let mut all = vec![fp(&["old"], &[]); 10];
        all.extend((0..MAX_FOOTPRINTS).map(|i| fp(&[&format!("t{i}")], &[])));
        assert_eq!(
            capacity(&all, DEFAULT_CLEAN_SHARE)
                .unwrap()
                .conflicting_pairs,
            0.0
        );
    }

    proptest! {
        #[test]
        fn shares_are_bounded_and_monotone(raw in proptest::collection::vec((0u8..6, 0u8..6), MIN_FOOTPRINTS..60)) {
            let all: Vec<Footprint> = raw.iter().map(|(w, r)| fp(&[&format!("t{w}")], &[&format!("t{r}")])).collect();
            let c = capacity(&all, DEFAULT_CLEAN_SHARE).unwrap();
            prop_assert!((0.0..=1.0).contains(&c.conflicting_pairs));
            prop_assert!(c.agents >= 1 && c.agents <= MAX_AGENTS);
            for w in c.curve.windows(2) {
                prop_assert!(w[1].clean_share <= w[0].clean_share + 1e-12);
                prop_assert!(w[1].textual_share + 1e-12 >= w[0].textual_share);
            }
            for p in &c.curve {
                prop_assert!((0.0..=1.0).contains(&p.clean_share));
                prop_assert!((0.0..=1.0).contains(&p.textual_share));
            }
            prop_assert_eq!(c.curve[0].clean_share, 1.0);
        }

        #[test]
        fn agrees_with_the_conflict_relation(raw in proptest::collection::vec((0u8..8, 0u8..8, 0u8..8), MIN_FOOTPRINTS..50)) {
            let all: Vec<Footprint> = raw
                .iter()
                .map(|(w, r1, r2)| fp(&[&format!("t{w}")], &[&format!("t{r1}"), &format!("t{r2}")]))
                .collect();
            let c = capacity(&all, DEFAULT_CLEAN_SHARE).unwrap();
            let n = all.len();
            let (mut conflicting, mut via) = (0u64, BTreeMap::<TargetId, u64>::new());
            for i in 0..n {
                for j in i + 1..n {
                    if all[i].relation(&all[j]) != sheltie_core::Relation::Independent {
                        conflicting += 1;
                        let shared: BTreeSet<&TargetId> = all[i].writes.intersection(&all[j].reads)
                            .chain(all[j].writes.intersection(&all[i].reads)).collect();
                        for t in shared {
                            *via.entry(t.clone()).or_default() += 1;
                        }
                    }
                }
            }
            let expected = conflicting as f64 / (n * (n - 1) / 2) as f64;
            prop_assert!((c.conflicting_pairs - expected).abs() < 1e-12);
            for h in &c.hot_targets {
                prop_assert_eq!(via.get(&h.target).copied(), Some(h.pairs));
            }
        }
    }
}
