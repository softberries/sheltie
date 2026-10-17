//! Tier 3 line and file-name merges.

use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;

/// Merges an order-insensitive file: lines removed on either side are removed, lines added on
/// either side are kept, ours first. Duplicate lines are dropped.
pub fn merge_set(base: &str, ours: &str, theirs: &str) -> String {
    let lines = |s: &str| s.lines().map(str::to_owned).collect::<Vec<_>>();
    let (b, o, t) = (lines(base), lines(ours), lines(theirs));
    let base_set: BTreeSet<&String> = b.iter().collect();
    let ours_set: BTreeSet<&String> = o.iter().collect();
    let theirs_set: BTreeSet<&String> = t.iter().collect();
    let removed: BTreeSet<&String> = base_set
        .iter()
        .filter(|l| !ours_set.contains(*l) || !theirs_set.contains(*l))
        .copied()
        .collect();
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for l in o.iter().chain(t.iter()) {
        if !removed.contains(l) && seen.insert(l.clone()) {
            out.push(l.clone());
        }
    }
    let mut text = out.join("\n");
    if ours.ends_with('\n') || (ours.is_empty() && theirs.ends_with('\n')) {
        text.push('\n');
    }
    text
}

/// Renames for numbered files: files the later side (`theirs`) added whose number collides
/// with an existing file or one ours added get the next free numbers, keeping the width.
/// Paths are repository paths; `key` captures the number in the file name.
pub fn renumber(
    existing: &[String],
    ours_added: &[String],
    theirs_added: &[String],
    key: &Regex,
) -> Vec<(String, String)> {
    let number = |path: &str| -> Option<(u64, usize, std::ops::Range<usize>)> {
        let name_start = path.rfind('/').map_or(0, |i| i + 1);
        let caps = key.captures(&path[name_start..])?;
        let m = caps.get(1)?;
        let n = m.as_str().parse().ok()?;
        Some((
            n,
            m.as_str().len(),
            name_start + m.start()..name_start + m.end(),
        ))
    };
    let mut taken: BTreeSet<u64> = existing
        .iter()
        .chain(ours_added)
        .filter_map(|p| number(p).map(|(n, ..)| n))
        .collect();
    let mut theirs: Vec<(u64, &String)> = theirs_added
        .iter()
        .filter_map(|p| number(p).map(|(n, ..)| (n, p)))
        .collect();
    theirs.sort();
    let mut renames = BTreeMap::new();
    for (n, path) in theirs {
        if taken.insert(n) {
            continue;
        }
        let next = taken.iter().next_back().map_or(1, |m| m + 1);
        taken.insert(next);
        if let Some((_, width, range)) = number(path) {
            let mut renamed = path.clone();
            renamed.replace_range(range, &format!("{next:0width$}"));
            renames.insert(path.clone(), renamed);
        }
    }
    renames.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn set_union_keeps_both_additions_and_both_removals() {
        let base = "a\nb\nc\n";
        let ours = "a\nc\nours\n"; // removed b
        let theirs = "a\nb\ntheirs\n"; // removed c
        assert_eq!(merge_set(base, ours, theirs), "a\nours\ntheirs\n");
    }

    #[test]
    fn set_union_drops_duplicates() {
        assert_eq!(merge_set("", "x\n", "x\ny\n"), "x\ny\n");
    }

    #[test]
    fn renumbers_the_later_side() {
        let key = Regex::new(r"^(\d+)_").unwrap();
        let existing = vec!["m/0001_init.sql".to_owned(), "m/0002_users.sql".to_owned()];
        let ours = vec!["m/0003_orders.sql".to_owned()];
        let theirs = vec![
            "m/0003_invoices.sql".to_owned(),
            "m/0004_taxes.sql".to_owned(),
        ];
        assert_eq!(
            renumber(&existing, &ours, &theirs, &key),
            vec![
                (
                    "m/0003_invoices.sql".to_owned(),
                    "m/0004_invoices.sql".to_owned()
                ),
                ("m/0004_taxes.sql".to_owned(), "m/0005_taxes.sql".to_owned())
            ]
        );
    }

    #[test]
    fn no_collision_no_rename() {
        let key = Regex::new(r"^(\d+)_").unwrap();
        assert!(renumber(&[], &["m/1_a.sql".into()], &["m/2_b.sql".into()], &key).is_empty());
    }

    proptest! {
        #[test]
        fn set_merge_contains_every_addition(
            base in prop::collection::btree_set("[a-e]", 0..5),
            add_o in prop::collection::btree_set("[f-j]", 0..3),
            add_t in prop::collection::btree_set("[k-o]", 0..3),
        ) {
            let join = |s: &BTreeSet<String>| s.iter().map(|l| format!("{l}\n")).collect::<String>();
            let ours: BTreeSet<String> = base.union(&add_o).cloned().collect();
            let theirs: BTreeSet<String> = base.union(&add_t).cloned().collect();
            let merged = merge_set(&join(&base), &join(&ours), &join(&theirs));
            let lines: BTreeSet<String> = merged.lines().map(str::to_owned).collect();
            let expected: BTreeSet<String> = base.iter().chain(&add_o).chain(&add_t).cloned().collect();
            prop_assert_eq!(lines, expected);
        }

        #[test]
        fn renumbering_leaves_numbers_unique(
            existing in prop::collection::btree_set(1u64..20, 0..6),
            ours in prop::collection::btree_set(1u64..20, 0..4),
            theirs in prop::collection::btree_set(1u64..20, 0..4),
        ) {
            let key = Regex::new(r"^(\d+)_").unwrap();
            let paths = |s: &BTreeSet<u64>, tag: &str| s.iter().map(|n| format!("m/{n:03}_{tag}.sql")).collect::<Vec<_>>();
            let (e, o, t) = (paths(&existing, "e"), paths(&ours, "o"), paths(&theirs, "t"));
            let renames: BTreeMap<String, String> = renumber(&e, &o, &t, &key).into_iter().collect();
            let theirs_after: Vec<String> = t.iter().map(|p| renames.get(p).cloned().unwrap_or_else(|| p.clone())).collect();
            let mut numbers: Vec<u64> = e.iter().chain(&o).chain(&theirs_after)
                .map(|p| key.captures(&p[2..]).unwrap()[1].parse::<u64>().unwrap())
                .collect();
            let before = numbers.len();
            numbers.sort_unstable();
            numbers.dedup();
            // Only collisions between existing and ours may remain (those are not ours to fix).
            let unfixable = existing.intersection(&ours).count();
            prop_assert_eq!(numbers.len() + unfixable, before);
        }
    }
}
