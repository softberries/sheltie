//! Domain types for Sheltie: changes, footprints and the conflict relation.
//!
//! This crate is pure: no I/O, no async, no external services. Everything here must be
//! deterministic so it can be unit- and property-tested. See `docs/design.md`, "Core model".
//!
//! M0 provides the identifier newtypes and the conflict relation over target sets. The
//! remaining records (`Change`, `Verification`, `Resolution`, `HeadEvent`) are added in M0/M1
//! as described in `docs/backlog.md`.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Defines a string-backed identifier newtype.
macro_rules! id_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates the identifier from any string-like value.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Returns the identifier as a string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_newtype!(
    /// A build target: a crate, module or package, as named by a build-graph adapter.
    TargetId
);
id_newtype!(
    /// A test (or test target) that covers one or more build targets.
    TestId
);
id_newtype!(
    /// A git commit SHA.
    CommitSha
);
id_newtype!(
    /// A git tree hash; footprints are cached per base tree.
    TreeHash
);

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

#[cfg(test)]
mod tests {
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
}
