//! Prices: the standard VAT rate, discounts and rounding. Amounts are in cents.

/// The standard VAT rate, in percent.
pub fn rate() -> u32 {
    20
}

/// VAT on `net` cents at the standard rate, rounded half up.
pub fn tax(net: u64) -> u64 {
    (net * u64::from(rate()) + 50) / 100
}

/// `net` minus `percent` percent, rounded down.
pub fn discounted(net: u64, percent: u8) -> u64 {
    net - net * u64::from(percent.min(100)) / 100
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tax_is_rounded() {
        assert_eq!(tax(1_000), 200);
        assert_eq!(tax(1_002), 200);
        assert_eq!(tax(1_003), 201);
    }

    #[test]
    fn discounts() {
        assert_eq!(discounted(1_000, 10), 900);
        assert_eq!(discounted(1_000, 150), 0);
    }
}
