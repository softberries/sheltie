//! Identifier newtypes.

use std::fmt;
use std::str::FromStr;

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

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
    /// A path relative to the repository root, `/`-separated.
    RepoPath
);
id_newtype!(
    /// Content hash of a change's diff; footprints are cached by `(base tree, diff hash)`.
    DiffHash
);
id_newtype!(
    /// Reference to a stored `intent.json` blob attached to a change.
    IntentRef
);
id_newtype!(
    /// The CI runner (or worker) that produced a verification.
    RunnerId
);

/// Error returned when a git object id is not valid hex of the right length.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid git object id {0:?}: expected 40 (SHA-1) or 64 (SHA-256) lowercase hex digits")]
pub struct InvalidOid(String);

/// Defines a newtype for a git object id (SHA-1 or SHA-256, lowercase hex), validated on parse.
macro_rules! oid_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Parses a git object id: 40 or 64 lowercase hex digits.
            pub fn parse(value: impl Into<String>) -> Result<Self, InvalidOid> {
                let value = value.into();
                let valid_len = matches!(value.len(), 40 | 64);
                if valid_len && value.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
                    Ok(Self(value))
                } else {
                    Err(InvalidOid(value))
                }
            }

            /// Returns the object id as a string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = InvalidOid;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::parse(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl FromStr for $name {
            type Err = InvalidOid;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::parse(s)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

oid_newtype!(
    /// A git commit SHA.
    CommitSha
);
oid_newtype!(
    /// A git tree hash; footprints and verifications are keyed by tree.
    TreeHash
);

/// GitHub's numeric repository id; stable across renames and transfers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RepoId(pub u64);

/// A pull request number within a repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PrNumber(pub u64);

impl fmt::Display for RepoId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for PrNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Identifies one change: a pull request at a specific head commit.
///
/// A push to the PR produces a new head and therefore a new change; footprints and
/// verifications of the old head stay attached to the old id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ChangeId {
    /// Repository the pull request belongs to.
    pub repo: RepoId,
    /// Pull request number.
    pub pr: PrNumber,
    /// Head commit of the pull request.
    pub head: CommitSha,
}

impl fmt::Display for ChangeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let short = self.head.as_str().get(..12).unwrap_or(self.head.as_str());
        write!(f, "{}{}@{short}", self.repo, self.pr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA1: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn commit_sha_accepts_sha1_and_sha256() {
        assert!(CommitSha::parse(SHA1).is_ok());
        assert!(CommitSha::parse("a".repeat(64)).is_ok());
    }

    #[test]
    fn commit_sha_rejects_bad_input() {
        for bad in [
            "",
            "abc",
            &SHA1.to_uppercase(),
            &format!("{SHA1}0"),
            &"g".repeat(40),
        ] {
            assert!(CommitSha::parse(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn commit_sha_deserialization_validates() {
        let ok: CommitSha = serde_json::from_str(&format!("\"{SHA1}\"")).unwrap();
        assert_eq!(ok.as_str(), SHA1);
        assert!(serde_json::from_str::<CommitSha>("\"nope\"").is_err());
    }

    #[test]
    fn change_id_display_is_short() {
        let id = ChangeId {
            repo: RepoId(7),
            pr: PrNumber(42),
            head: CommitSha::parse(SHA1).unwrap(),
        };
        assert_eq!(id.to_string(), "7#42@0123456789ab");
    }
}
