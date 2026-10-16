//! CI state of a default-branch commit, from GitHub's status-check rollup.

use serde::Deserialize;
use sheltie_store::CiState;

/// The GraphQL query; variables `owner`, `name`, `oid`.
pub(crate) const QUERY: &str = r#"
query($owner: String!, $name: String!, $oid: GitObjectID!) {
  repository(owner: $owner, name: $name) {
    object(oid: $oid) {
      ... on Commit { statusCheckRollup { state } }
    }
  }
}"#;

/// The `data` of the query response.
#[derive(Debug, Deserialize)]
pub struct CommitData {
    repository: Option<Repo>,
}

#[derive(Debug, Deserialize)]
struct Repo {
    object: Option<Object>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Object {
    status_check_rollup: Option<Rollup>,
}

#[derive(Debug, Deserialize)]
struct Rollup {
    state: String,
}

/// Maps the rollup state. No rollup means the commit has no checks; `EXPECTED` (a required
/// check that has not reported) counts as pending.
pub fn evaluate(data: &CommitData) -> CiState {
    let rollup = data
        .repository
        .as_ref()
        .and_then(|r| r.object.as_ref())
        .and_then(|o| o.status_check_rollup.as_ref());
    match rollup.map(|r| r.state.as_str()) {
        None => CiState::None,
        Some("SUCCESS") => CiState::Success,
        Some("FAILURE") => CiState::Failure,
        Some("ERROR") => CiState::Error,
        Some(_) => CiState::Pending,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn data(rollup: serde_json::Value) -> CommitData {
        serde_json::from_value(
            json!({ "repository": { "object": { "statusCheckRollup": rollup } } }),
        )
        .unwrap()
    }

    #[test]
    fn states() {
        assert_eq!(
            evaluate(&data(json!({ "state": "SUCCESS" }))),
            CiState::Success
        );
        assert_eq!(
            evaluate(&data(json!({ "state": "FAILURE" }))),
            CiState::Failure
        );
        assert_eq!(evaluate(&data(json!({ "state": "ERROR" }))), CiState::Error);
        assert_eq!(
            evaluate(&data(json!({ "state": "EXPECTED" }))),
            CiState::Pending
        );
        assert_eq!(
            evaluate(&data(json!({ "state": "PENDING" }))),
            CiState::Pending
        );
        assert_eq!(evaluate(&data(serde_json::Value::Null)), CiState::None);
    }
}
