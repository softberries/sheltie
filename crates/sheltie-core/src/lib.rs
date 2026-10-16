//! Domain types for Sheltie: changes, footprints, verifications, resolutions and head events.
//!
//! This crate is pure: no I/O, no async, no external services. Everything here must be
//! deterministic so it can be unit- and property-tested. See `docs/design.md`, "Core model"
//! and "Data model and APIs".

/// Defines a fieldless enum with a stable `snake_case` text form, used for serde and storage.
///
/// Generates `ALL`, `as_str`, `Display` and `FromStr`. The text forms are part of the database
/// schema (`CHECK` constraints in `sheltie-store`), so renaming a variant needs a migration.
macro_rules! text_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident => $text:literal, )+
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        $vis enum $name {
            $( $(#[$vmeta])* #[serde(rename = $text)] $variant, )+
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// The stable text form of this variant.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $text,)+
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl std::str::FromStr for $name {
            type Err = $crate::UnknownVariant;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($text => Ok($name::$variant),)+
                    other => Err($crate::UnknownVariant {
                        kind: stringify!($name),
                        value: other.to_owned(),
                    }),
                }
            }
        }
    };
}

mod change;
mod footprint;
mod head;
mod ids;
pub mod intake;
mod resolution;
mod score;
mod verification;

pub use change::{Change, ChangeState};
pub use footprint::{ChangeFootprint, Footprint, Relation};
pub use head::{GuaranteeTier, HeadEvent};
pub use ids::{
    ChangeId, CommitSha, DeliveryId, DiffHash, InstallationId, IntentRef, InvalidOid,
    InvalidRepoName, Login, PrNumber, RepoFullName, RepoId, RepoPath, RunnerId, TargetId, TestId,
    TreeHash,
};
pub use resolution::{Resolution, ResolutionTier};
pub use score::{InvalidScore, Score};
pub use verification::{Outcome, Verification};

/// Error returned when parsing an enum from text that names no variant.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown {kind} {value:?}")]
pub struct UnknownVariant {
    /// The enum being parsed.
    pub kind: &'static str,
    /// The rejected text.
    pub value: String,
}
