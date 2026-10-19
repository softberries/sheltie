//! The merge engine: resolution tiers 1–3 and the tier-4 replay client.
//!
//! Runs on the customer's CI runner, never in the control plane: it needs a checkout, git,
//! the build tools for regenerating derived files, and mergiraf for structural merges.
//! mergiraf is GPL-3.0 and is only ever invoked as a separate program.

pub mod acceptance;
pub mod engine;
pub mod lines;
pub mod replay;
pub mod rules;
