//! Footprint prediction before any code is written (coordination, M7).
//!
//! A planner names the files or modules a task will touch, or Sheltie reads them from an
//! issue; [`predict`] turns them into a footprint against the default branch's snapshot.
//! Prediction is pessimistic: written targets are assumed to change their interfaces, so
//! their direct readers are in the read set.

use std::collections::BTreeSet;

use sheltie_core::{Footprint, RepoPath, TargetId};

use crate::{GraphSnapshot, IfaceHash, expand};

/// What a task is expected to touch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Touches {
    /// Files.
    pub paths: BTreeSet<RepoPath>,
    /// Targets named directly.
    pub targets: BTreeSet<TargetId>,
}

impl Touches {
    /// Nothing recognised.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.targets.is_empty()
    }
}

/// A predicted footprint, or `None` when nothing in `touches` maps to the graph.
///
/// A path no target owns (a new file) is attributed to the owners of other files in its
/// directory; if there are none either, the path is ignored.
pub fn predict(snapshot: &GraphSnapshot, touches: &Touches) -> Option<Footprint> {
    let mut writes: BTreeSet<TargetId> = touches
        .targets
        .iter()
        .filter(|t| snapshot.targets.contains_key(*t))
        .cloned()
        .collect();
    for path in &touches.paths {
        match snapshot.owners.get(path) {
            Some(owners) => writes.extend(owners.iter().cloned()),
            None => {
                let dir = parent(path.as_str());
                writes.extend(
                    snapshot
                        .owners
                        .iter()
                        .filter(|(p, _)| parent(p.as_str()) == dir)
                        .flat_map(|(_, o)| o.iter().cloned()),
                );
            }
        }
    }
    if writes.is_empty() {
        return None;
    }
    let mut changed = snapshot.clone();
    for t in &writes {
        if let Some(target) = changed.targets.get_mut(t) {
            target.iface = IfaceHash(format!("{}~predicted", target.iface.0));
            target.public_iface = IfaceHash(format!("{}~predicted", target.public_iface.0));
        }
    }
    Some(expand(snapshot, &changed, writes, false, &BTreeSet::new()))
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// Most bytes of text examined by [`touches_in_text`].
const MAX_TEXT: usize = 64 * 1024;

/// Finds what an issue's text refers to in the graph:
/// - file paths that the snapshot knows, and directories (every file under them);
/// - target ids, in full (`shop::cart`) or by a `::` suffix (`cart::totals`);
/// - identifiers in backticks (`` `cart` ``) equal to the last segment of a target id.
///
/// Bare words are ignored: they match too much by accident.
pub fn touches_in_text(snapshot: &GraphSnapshot, text: &str) -> Touches {
    let text: String = text.chars().take(MAX_TEXT).collect();
    let mut found = Touches::default();
    let trim = |w: &str| {
        w.trim_matches(|c: char| {
            !(c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '.' | ':' | '-'))
        })
        .trim_end_matches(['.', ':'])
        .trim_start_matches("./")
        .to_owned()
    };
    for raw in text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                ',' | ';' | '(' | ')' | '[' | ']' | '"' | '\'' | '<' | '>'
            )
    }) {
        let backticked = raw.starts_with('`')
            && raw
                .trim_end_matches(|c: char| c.is_ascii_punctuation() && c != '`')
                .ends_with('`');
        let word = trim(raw);
        if word.is_empty() {
            continue;
        }
        if word.contains('/') || word.contains('.') && !word.contains("::") {
            let prefix = format!("{}/", word.trim_end_matches('/'));
            for path in snapshot.owners.keys() {
                if path.as_str() == word || path.as_str().starts_with(&prefix) {
                    found.paths.insert(path.clone());
                }
            }
            continue;
        }
        if word.contains("::") {
            let suffix = format!("::{word}");
            for id in snapshot.targets.keys() {
                if id.as_str() == word || id.as_str().ends_with(&suffix) {
                    found.targets.insert(id.clone());
                }
            }
            continue;
        }
        if backticked {
            let suffix = format!("::{word}");
            for id in snapshot.targets.keys() {
                if id.as_str().ends_with(&suffix) || id.as_str() == word {
                    found.targets.insert(id.clone());
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::{EdgeKind, Target, Zone};

    fn target(reads: &[&str], hermetic: bool) -> Target {
        Target {
            zone: Zone::Group("shop".into()),
            iface: IfaceHash("i".into()),
            public_iface: IfaceHash("p".into()),
            reads: reads
                .iter()
                .map(|r| (TargetId::new(*r), EdgeKind::Local))
                .collect(),
            tests: BTreeSet::new(),
            hermetic,
        }
    }

    /// shop::cart reads shop::pricing; shop::catalog stands alone.
    fn snapshot() -> GraphSnapshot {
        GraphSnapshot {
            targets: BTreeMap::from([
                (
                    TargetId::new("shop::cart"),
                    target(&["shop::pricing"], true),
                ),
                (TargetId::new("shop::pricing"), target(&[], true)),
                (TargetId::new("shop::catalog"), target(&[], true)),
            ]),
            owners: BTreeMap::from([
                (
                    RepoPath::new("src/cart.rs"),
                    BTreeSet::from([TargetId::new("shop::cart")]),
                ),
                (
                    RepoPath::new("src/pricing/mod.rs"),
                    BTreeSet::from([TargetId::new("shop::pricing")]),
                ),
                (
                    RepoPath::new("src/pricing/rules.rs"),
                    BTreeSet::from([TargetId::new("shop::pricing")]),
                ),
                (
                    RepoPath::new("src/catalog.rs"),
                    BTreeSet::from([TargetId::new("shop::catalog")]),
                ),
            ]),
        }
    }

    fn ids(set: &BTreeSet<TargetId>) -> Vec<&str> {
        set.iter().map(TargetId::as_str).collect()
    }

    #[test]
    fn writes_reach_direct_readers() {
        let s = snapshot();
        let touches = Touches {
            paths: BTreeSet::from([RepoPath::new("src/pricing/rules.rs")]),
            ..Touches::default()
        };
        let f = predict(&s, &touches).unwrap();
        assert_eq!(ids(&f.writes), ["shop::pricing"]);
        assert_eq!(ids(&f.reads), ["shop::cart", "shop::pricing"]);
        assert!(!f.leaky);
    }

    #[test]
    fn new_files_belong_to_their_directory() {
        let s = snapshot();
        let touches = Touches {
            paths: BTreeSet::from([RepoPath::new("src/pricing/tax.rs")]),
            ..Touches::default()
        };
        assert_eq!(
            ids(&predict(&s, &touches).unwrap().writes),
            ["shop::pricing"]
        );
        let unknown = Touches {
            paths: BTreeSet::from([RepoPath::new("docs/x.md")]),
            ..Touches::default()
        };
        assert_eq!(predict(&s, &unknown), None);
    }

    #[test]
    fn text_mentions() {
        let s = snapshot();
        let t = touches_in_text(
            &s,
            "Fix rounding in src/pricing/ and `cart`; see shop::catalog.",
        );
        assert_eq!(
            t.paths.iter().map(RepoPath::as_str).collect::<Vec<_>>(),
            ["src/pricing/mod.rs", "src/pricing/rules.rs"]
        );
        assert_eq!(ids(&t.targets), ["shop::cart", "shop::catalog"]);
        assert!(
            touches_in_text(&s, "The cart is slow sometimes.").is_empty(),
            "bare words are ignored"
        );
        let t = touches_in_text(&s, "Update (src/cart.rs).");
        assert_eq!(t.paths.len(), 1);
    }

    #[test]
    fn overlapping_issues_conflict() {
        use sheltie_core::Relation;
        let s = snapshot();
        let a = predict(&s, &touches_in_text(&s, "Change `pricing` rules")).unwrap();
        let b = predict(&s, &touches_in_text(&s, "Cart: edit src/cart.rs")).unwrap();
        let c = predict(&s, &touches_in_text(&s, "Catalog: edit src/catalog.rs")).unwrap();
        assert_eq!(a.relation(&b), Relation::Semantic, "cart reads pricing");
        assert_eq!(a.relation(&c), Relation::Independent);
    }
}
