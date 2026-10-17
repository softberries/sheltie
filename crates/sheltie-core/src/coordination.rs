//! Coordination before code is written: which issues can be worked on in parallel (M7).
//!
//! A lease reserves an issue's predicted footprint for one agent. Two leases may coexist only
//! when their footprints are [`Relation::Independent`], so agents given overlapping issues are
//! serialized.

use serde::{Deserialize, Serialize};

use crate::footprint::{Footprint, Relation};

/// An issue number within a repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IssueNumber(pub u64);

impl std::fmt::Display for IssueNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Issues a pull request description closes with GitHub's keywords (`Fixes #12`,
/// `closes #3`, `Resolved: #4`), in this repository only.
pub fn closing_references(text: &str) -> std::collections::BTreeSet<IssueNumber> {
    const KEYWORDS: [&str; 9] = [
        "close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved",
    ];
    let words: Vec<&str> = text.split_whitespace().take(20_000).collect();
    let mut found = std::collections::BTreeSet::new();
    for pair in words.windows(2) {
        let keyword = pair[0].trim_end_matches(':').to_ascii_lowercase();
        if !KEYWORDS.contains(&keyword.as_str()) {
            continue;
        }
        let Some(number) = pair[1].strip_prefix('#') else {
            continue;
        };
        let digits: String = number.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(n) = digits.parse::<u64>()
            && n > 0
        {
            found.insert(IssueNumber(n));
        }
    }
    found
}

/// The first held footprint that `wanted` conflicts with, if any.
pub fn first_conflict<'a, K>(wanted: &Footprint, held: &'a [(K, Footprint)]) -> Option<&'a K> {
    held.iter()
        .find(|(_, f)| wanted.relation(f) != Relation::Independent)
        .map(|(k, _)| k)
}

/// Picks candidates, in order, that conflict neither with `held` nor with each other.
/// Candidates without a footprint (`None`, nothing could be predicted) are never picked.
pub fn safe_set<K: Clone>(
    held: &[(K, Footprint)],
    candidates: &[(K, Option<Footprint>)],
) -> Vec<K> {
    let mut taken: Vec<(K, Footprint)> = held.to_vec();
    let mut safe = Vec::new();
    for (key, footprint) in candidates {
        let Some(footprint) = footprint else { continue };
        if first_conflict(footprint, &taken).is_none() {
            taken.push((key.clone(), footprint.clone()));
            safe.push(key.clone());
        }
    }
    safe
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use proptest::prelude::*;

    use super::*;
    use crate::ids::TargetId;

    fn fp(writes: &[&str], reads: &[&str]) -> Footprint {
        let set = |xs: &[&str]| {
            xs.iter()
                .map(|x| TargetId::new(*x))
                .collect::<BTreeSet<_>>()
        };
        let mut reads = set(reads);
        reads.extend(set(writes));
        Footprint {
            writes: set(writes),
            reads,
            ..Footprint::default()
        }
    }

    #[test]
    fn closing_keywords() {
        let refs = closing_references(
            "Fixes #12, closes #3.\nResolved: #4\nSee #5; fix acme/x#6; Fixes #0",
        );
        assert_eq!(
            refs.into_iter().map(|i| i.0).collect::<Vec<_>>(),
            [3, 4, 12]
        );
    }

    #[test]
    fn conflicts_and_safe_sets() {
        let held = [(1, fp(&["pricing"], &["cart"]))];
        assert_eq!(
            first_conflict(&fp(&["cart"], &[]), &held),
            Some(&1),
            "writes what the lease reads"
        );
        assert_eq!(first_conflict(&fp(&["catalog"], &[]), &held), None);
        let candidates = [
            (2, Some(fp(&["catalog"], &[]))),
            (3, Some(fp(&["catalog"], &[]))),
            (4, None),
            (5, Some(fp(&["search"], &[]))),
            (6, Some(fp(&["pricing"], &[]))),
        ];
        assert_eq!(safe_set(&held, &candidates), [2, 5]);
    }

    fn arb_fp() -> impl Strategy<Value = Footprint> {
        let names = prop::collection::vec(0u8..8, 0..3);
        (names.clone(), names).prop_map(|(w, r)| {
            let w: Vec<String> = w.iter().map(|i| format!("t{i}")).collect();
            let r: Vec<String> = r.iter().map(|i| format!("t{i}")).collect();
            fp(
                &w.iter().map(String::as_str).collect::<Vec<_>>(),
                &r.iter().map(String::as_str).collect::<Vec<_>>(),
            )
        })
    }

    proptest! {
        /// Whatever is picked is independent of everything held and of everything else picked.
        #[test]
        fn safe_sets_are_pairwise_independent(
            held in prop::collection::vec(arb_fp(), 0..4),
            cands in prop::collection::vec(prop::option::of(arb_fp()), 0..8),
        ) {
            let held: Vec<(usize, Footprint)> = held.into_iter().enumerate().collect();
            let cands: Vec<(usize, Option<Footprint>)> = cands.into_iter().enumerate().map(|(i, f)| (100 + i, f)).collect();
            let picked = safe_set(&held, &cands);
            let fps: Vec<&Footprint> = picked.iter().map(|k| cands[k - 100].1.as_ref().unwrap()).collect();
            for (i, a) in fps.iter().enumerate() {
                for (_, h) in &held {
                    prop_assert_eq!(a.relation(h), Relation::Independent);
                }
                for b in &fps[i + 1..] {
                    prop_assert_eq!(a.relation(b), Relation::Independent);
                }
            }
        }
    }
}
