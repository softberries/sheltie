//! GitHub App integration: webhook verification and parsing, the App client, and check runs.
//!
//! Inbound: [`WebhookSecret::verify`] then [`payload::parse`] turn a webhook into an
//! [`Intake`](sheltie_core::intake::Intake) event for the store. Outbound: [`CheckRunSync`]
//! pushes each change's state to its "Sheltie" check run through [`GitHubApp`].

pub mod check_run;
mod client;
pub mod oidc;
pub mod payload;
mod signature;
mod sync;
#[cfg(feature = "test-support")]
pub mod testing;

pub use client::{GitHubApp, GitHubError};
pub use signature::{SignatureError, WebhookSecret};
pub use sync::CheckRunSync;
