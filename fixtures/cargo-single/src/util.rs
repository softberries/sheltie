//! Helpers.

/// Rounds cents up to a multiple of five.
pub(crate) fn round5(cents: u64) -> u64 {
    cents.div_ceil(5) * 5
}
