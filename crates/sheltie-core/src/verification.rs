//! Verification records.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::ids::{RunnerId, TargetId, TestId, TreeHash};

/// The result of building and testing a target set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", content = "tests", rename_all = "snake_case")]
pub enum Outcome {
    /// Every target built and every covering test passed.
    Pass,
    /// These tests failed.
    Fail(BTreeSet<TestId>),
    /// Results were inconsistent across retries; not evidence either way.
    Flaky,
}

/// A verification of a target set on a tree.
///
/// Reusable by any later change whose relevant inputs hash to the same tree and targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verification {
    /// The tree that was built.
    pub tree: TreeHash,
    /// The targets that were built and tested.
    pub targets: BTreeSet<TargetId>,
    /// What happened.
    pub outcome: Outcome,
    /// Where it ran.
    pub runner: RunnerId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_serde_shape() {
        assert_eq!(
            serde_json::to_string(&Outcome::Pass).unwrap(),
            r#"{"result":"pass"}"#
        );
        let fail = Outcome::Fail(BTreeSet::from([TestId::new("auth::login")]));
        let json = serde_json::to_string(&fail).unwrap();
        assert_eq!(json, r#"{"result":"fail","tests":["auth::login"]}"#);
        assert_eq!(serde_json::from_str::<Outcome>(&json).unwrap(), fail);
    }
}
