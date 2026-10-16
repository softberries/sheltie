//! A tiny shop: one crate, several modules.

#[macro_use]
mod macros;

pub mod cart;
pub mod catalog;
pub mod pricing;
pub mod shipping;
mod util;

pub use cart::Cart;
