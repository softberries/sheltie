//! Footprints and the conflict relation between them.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::ids::{ChangeId, CommitSha, DiffHash, TargetId, TestId, TreeHash};

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

/// Limits on an uploaded [`FootprintReport`]; uploads come from CI and are untrusted.
pub mod limits {
    /// Most targets (or tests) in one set.
    pub const MAX_SET: usize = 100_000;
    /// Longest target, test or adapter name.
    pub const MAX_NAME: usize = 512;
    /// Most leak reasons.
    pub const MAX_REASONS: usize = 1_000;
}

/// The footprint of one change as an adapter uploads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FootprintReport {
    /// Adapter name and version, e.g. `sheltie-cargo/0.1.0`.
    pub adapter: String,
    /// The commit the footprint was computed against: the merge base of base and head.
    pub base: CommitSha,
    /// The change's head commit.
    pub head: CommitSha,
    /// Tree of `base`.
    pub base_tree: TreeHash,
    /// Hash of the diff from `base` to `head`.
    pub diff_hash: DiffHash,
    /// The write and read sets.
    pub footprint: Footprint,
    /// Why the footprint is leaky, if it is.
    pub leak_reasons: Vec<String>,
}

/// Why an uploaded report was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid footprint report: {0}")]
pub struct InvalidReport(pub String);

impl FootprintReport {
    /// Checks sizes and invariants: names are short printable text, sets are bounded,
    /// and every written target is also read.
    pub fn validate(&self) -> Result<(), InvalidReport> {
        use limits::*;
        let name_ok =
            |s: &str| !s.is_empty() && s.len() <= MAX_NAME && !s.chars().any(char::is_control);
        let fail = |m: String| Err(InvalidReport(m));
        if !name_ok(&self.adapter) {
            return fail("adapter name".into());
        }
        if !name_ok(self.diff_hash.as_str()) {
            return fail("diff hash".into());
        }
        let fp = &self.footprint;
        for (what, len) in [
            ("writes", fp.writes.len()),
            ("reads", fp.reads.len()),
            ("tests", fp.tests.len()),
        ] {
            if len > MAX_SET {
                return fail(format!("{what} has {len} entries (max {MAX_SET})"));
            }
        }
        if let Some(bad) = fp
            .writes
            .iter()
            .chain(&fp.reads)
            .find(|t| !name_ok(t.as_str()))
        {
            return fail(format!(
                "target name {:?}",
                bad.as_str().chars().take(80).collect::<String>()
            ));
        }
        if let Some(bad) = fp.tests.iter().find(|t| !name_ok(t.as_str())) {
            return fail(format!(
                "test name {:?}",
                bad.as_str().chars().take(80).collect::<String>()
            ));
        }
        if !fp.writes.is_subset(&fp.reads) {
            return fail("writes must be a subset of reads".into());
        }
        if self.leak_reasons.len() > MAX_REASONS
            || self.leak_reasons.iter().any(|r| r.len() > MAX_NAME * 2)
        {
            return fail("too many or too long leak reasons".into());
        }
        Ok(())
    }
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

    fn report() -> FootprintReport {
        let sha = |c: char| c.to_string().repeat(40);
        FootprintReport {
            adapter: "sheltie-cargo/0.0.0".into(),
            base: CommitSha::parse(sha('a')).unwrap(),
            head: CommitSha::parse(sha('b')).unwrap(),
            base_tree: TreeHash::parse(sha('c')).unwrap(),
            diff_hash: DiffHash::new("sha256:00"),
            footprint: fp(&["x"], &["x", "y"]),
            leak_reasons: vec![],
        }
    }

    #[test]
    fn valid_report_passes() {
        assert_eq!(report().validate(), Ok(()));
    }

    #[test]
    fn writes_outside_reads_are_rejected() {
        let mut r = report();
        r.footprint = fp(&["x"], &["y"]);
        assert!(r.validate().is_err());
    }

    #[test]
    fn oversized_or_odd_names_are_rejected() {
        let mut r = report();
        r.footprint
            .reads
            .insert(TargetId::new("a".repeat(limits::MAX_NAME + 1)));
        assert!(r.validate().is_err());
        let mut r = report();
        r.adapter = "bad\nname".into();
        assert!(r.validate().is_err());
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
