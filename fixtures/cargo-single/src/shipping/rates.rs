//! Shipping rates.

use super::*;

/// Shipping cost in cents for a parcel.
pub fn rate(w: Weight) -> u64 {
    if w < 1000 { 300 } else { 300 + u64::from(w / 1000) * 150 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heavy_parcels_cost_more() {
        assert!(rate(5000) > rate(10));
    }
}
