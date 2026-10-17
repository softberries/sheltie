//! Optimistic commit check and commit naming.
//!
//! The committer lands a change on the default branch through a compare-and-swap: it reads
//! the head H, checks the change against what landed between the change's base and H
//! ([`check`]), and fast-forwards the branch from H to a merge commit of H and the change.
//! A lost race (the branch moved) costs another [`check`], not a build. This crate holds the
//! pure parts; the GitHub calls live in `sheltie-github`.
//!
//! See "Verification and commit protocol" in `docs/design.md`.

use std::collections::BTreeSet;

use sheltie_core::{CommitSha, Footprint, GuaranteeTier, PrNumber, TargetId};

/// What the commit check says about landing a change on the current head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitCheck {
    /// Nothing that landed since the base touches the change: land it as is.
    Commute,
    /// Something that landed writes targets the change reads: rebuild these first.
    Reverify(BTreeSet<TargetId>),
    /// Something that landed writes targets the change writes: it cannot land as is.
    WriteConflict(BTreeSet<TargetId>),
    /// What landed is not fully known (a direct push, or a change without footprint).
    Unknown,
}

/// Checks a change against the union of write sets that landed since its base (`None` when
/// that is unknown). Steps 3–5 of the commit protocol.
pub fn check(change: &Footprint, landed_writes: Option<&BTreeSet<TargetId>>) -> CommitCheck {
    let Some(landed) = landed_writes else {
        return CommitCheck::Unknown;
    };
    let written: BTreeSet<TargetId> = landed.intersection(&change.writes).cloned().collect();
    if !written.is_empty() {
        return CommitCheck::WriteConflict(written);
    }
    let read: BTreeSet<TargetId> = landed.intersection(&change.reads).cloned().collect();
    if read.is_empty() {
        CommitCheck::Commute
    } else {
        CommitCheck::Reverify(read)
    }
}

/// The message of the merge commit that lands a change, with Sheltie's trailers:
///
/// ```text
/// Merge #12: Add retry to the billing client
///
/// Sheltie-Verified: footprint S=ab12cd34ef56
/// Sheltie-Change: #12
/// ```
pub fn merge_message(pr: PrNumber, title: &str, tier: GuaranteeTier, base: &CommitSha) -> String {
    // A title is one line of user text; keep it short and on one line.
    let title: String = title
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(200)
        .collect();
    let short = base.as_str().get(..12).unwrap_or(base.as_str());
    format!(
        "Merge {pr}: {}\n\nSheltie-Verified: {tier} S={short}\nSheltie-Change: {pr}\n",
        title.trim()
    )
}

/// Temporary branch on which a change is merged before the default branch moves to it.
pub fn land_branch(pr: PrNumber, head: &CommitSha) -> String {
    format!(
        "sheltie/land/{}-{}",
        pr.0,
        &head.as_str()[..12.min(head.as_str().len())]
    )
}

/// Branch on which a change is merged onto the head and verified by the repository's CI.
pub fn verify_branch(pr: PrNumber, head: &CommitSha) -> String {
    format!(
        "sheltie/verify/{}-{}",
        pr.0,
        &head.as_str()[..12.min(head.as_str().len())]
    )
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn set(v: &[&str]) -> BTreeSet<TargetId> {
        v.iter().map(|t| TargetId::new(*t)).collect()
    }

    fn fp(writes: &[&str], reads: &[&str]) -> Footprint {
        let mut r = set(reads);
        r.extend(set(writes));
        Footprint {
            writes: set(writes),
            reads: r,
            tests: BTreeSet::new(),
            leaky: false,
        }
    }

    #[test]
    fn outcomes() {
        let c = fp(&["a"], &["b"]);
        assert_eq!(check(&c, Some(&set(&["z"]))), CommitCheck::Commute);
        assert_eq!(
            check(&c, Some(&set(&["b", "z"]))),
            CommitCheck::Reverify(set(&["b"]))
        );
        assert_eq!(
            check(&c, Some(&set(&["a", "b"]))),
            CommitCheck::WriteConflict(set(&["a"]))
        );
        assert_eq!(check(&c, None), CommitCheck::Unknown);
        assert_eq!(check(&c, Some(&BTreeSet::new())), CommitCheck::Commute);
    }

    #[test]
    fn message_has_trailers() {
        let base = CommitSha::parse("ab12cd34ef56".repeat(3) + "abcd").unwrap();
        let msg = merge_message(
            PrNumber(12),
            "Add retry\nto billing",
            GuaranteeTier::FootprintProven,
            &base,
        );
        assert_eq!(
            msg,
            "Merge #12: Add retry to billing\n\nSheltie-Verified: footprint S=ab12cd34ef56\nSheltie-Change: #12\n"
        );
    }

    #[test]
    fn branch_names() {
        let head = CommitSha::parse("1".repeat(40)).unwrap();
        assert_eq!(
            land_branch(PrNumber(7), &head),
            "sheltie/land/7-111111111111"
        );
        assert_eq!(
            verify_branch(PrNumber(7), &head),
            "sheltie/verify/7-111111111111"
        );
    }

    fn arb_set() -> impl Strategy<Value = BTreeSet<TargetId>> {
        prop::collection::btree_set("[a-f]", 0..4)
            .prop_map(|s| s.into_iter().map(TargetId::new).collect())
    }

    proptest! {
        #[test]
        fn commute_iff_disjoint(writes in arb_set(), reads in arb_set(), landed in arb_set()) {
            let mut all_reads = reads.clone();
            all_reads.extend(writes.iter().cloned());
            let c = Footprint { writes: writes.clone(), reads: all_reads.clone(), tests: BTreeSet::new(), leaky: false };
            let disjoint = landed.is_disjoint(&writes) && landed.is_disjoint(&all_reads);
            prop_assert_eq!(check(&c, Some(&landed)) == CommitCheck::Commute, disjoint);
        }

        #[test]
        fn write_conflict_wins_over_reverify(writes in arb_set(), landed in arb_set()) {
            let c = Footprint { writes: writes.clone(), reads: writes.clone(), tests: BTreeSet::new(), leaky: false };
            let outcome = check(&c, Some(&landed));
            if !landed.is_disjoint(&writes) {
                prop_assert!(matches!(outcome, CommitCheck::WriteConflict(_)));
            }
        }
    }
}
