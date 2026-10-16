//! Footprints and the conflict relation between them.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::ids::{ChangeId, TargetId, TestId, TreeHash};

/// The write and read sets of one change, computed against its base tree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Footprint {
    /// Targets whose sources the change modifies.
    pub writes: BTreeSet<TargetId>,
    /// Targets affected through a changed interface, including the written ones.
    pub reads: BTreeSet<TargetId>,
    /// Tests that must pass for this change.
    pub tests: BTreeSet<TestId>,
    /// True when a non-hermetic target widened the read set to a taint zone.
    pub leaky: bool,
}

/// How two changes relate, per the conflict relation in `docs/design.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Relation {
    /// Both changes write the same target; git may not apply both.
    Textual,
    /// One change writes a target the other reads; both apply but one may break the other.
    Semantic,
    /// Neither writes what the other reads or writes: the changes commute.
    Independent,
}

impl Footprint {
    /// Classifies how this footprint relates to another.
    ///
    /// Textual takes precedence over semantic. The relation is symmetric.
    pub fn relation(&self, other: &Footprint) -> Relation {
        if !self.writes.is_disjoint(&other.writes) {
            Relation::Textual
        } else if !self.writes.is_disjoint(&other.reads) || !other.writes.is_disjoint(&self.reads) {
            Relation::Semantic
        } else {
            Relation::Independent
        }
    }
}

/// A footprint as stored: attached to a change and the base tree it was computed against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeFootprint {
    /// The change this footprint belongs to.
    pub change: ChangeId,
    /// Tree of the base commit the footprint was computed against.
    pub base_tree: TreeHash,
    /// The write and read sets.
    pub footprint: Footprint,
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn fp(writes: &[&str], reads: &[&str]) -> Footprint {
        Footprint {
            writes: writes.iter().map(|t| TargetId::new(*t)).collect(),
            reads: reads.iter().map(|t| TargetId::new(*t)).collect(),
            ..Footprint::default()
        }
    }

    #[test]
    fn disjoint_footprints_are_independent() {
        let a = fp(&["auth"], &["auth"]);
        let b = fp(&["billing"], &["billing", "api"]);
        assert_eq!(a.relation(&b), Relation::Independent);
    }

    #[test]
    fn shared_write_is_textual() {
        let a = fp(&["api"], &["api"]);
        let b = fp(&["api"], &["api"]);
        assert_eq!(a.relation(&b), Relation::Textual);
    }

    #[test]
    fn write_into_other_read_set_is_semantic_both_ways() {
        let a = fp(&["core"], &["core", "api"]);
        let b = fp(&["api"], &["api"]);
        assert_eq!(a.relation(&b), Relation::Semantic);
        assert_eq!(b.relation(&a), Relation::Semantic);
    }

    #[test]
    fn shared_reads_alone_do_not_conflict() {
        let a = fp(&["auth"], &["auth", "api"]);
        let b = fp(&["billing"], &["billing", "api"]);
        assert_eq!(a.relation(&b), Relation::Independent);
    }

    /// Footprints over a small target alphabet, so overlaps are common.
    fn arb_footprint() -> impl Strategy<Value = Footprint> {
        let set = || prop::collection::btree_set("[a-f]", 0..4);
        (set(), set(), any::<bool>()).prop_map(|(writes, reads, leaky)| Footprint {
            writes: writes.into_iter().map(TargetId::new).collect(),
            reads: reads.into_iter().map(TargetId::new).collect(),
            tests: BTreeSet::new(),
            leaky,
        })
    }

    proptest! {
        #[test]
        fn relation_is_symmetric(a in arb_footprint(), b in arb_footprint()) {
            prop_assert_eq!(a.relation(&b), b.relation(&a));
        }

        #[test]
        fn independent_iff_writes_touch_nothing_of_the_other(
            a in arb_footprint(),
            b in arb_footprint(),
        ) {
            let touches = |x: &Footprint, y: &Footprint| {
                x.writes.iter().any(|t| y.writes.contains(t) || y.reads.contains(t))
            };
            let independent = !touches(&a, &b) && !touches(&b, &a);
            prop_assert_eq!(a.relation(&b) == Relation::Independent, independent);
        }

        #[test]
        fn textual_iff_writes_overlap(a in arb_footprint(), b in arb_footprint()) {
            let overlap = !a.writes.is_disjoint(&b.writes);
            prop_assert_eq!(a.relation(&b) == Relation::Textual, overlap);
        }

        #[test]
        fn footprint_serde_round_trips(a in arb_footprint()) {
            let json = serde_json::to_string(&a).unwrap();
            prop_assert_eq!(serde_json::from_str::<Footprint>(&json).unwrap(), a);
        }
    }
}
