//! Tax rules.

use ledger_core::Money;

/// Standard tax rate in percent.
pub const STANDARD_RATE: i64 = 20;

/// Tax due on a net amount.
pub fn tax_on(net: Money) -> Money {
    net.percent(STANDARD_RATE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_rate() {
        assert_eq!(tax_on(Money(1000)), Money(200));
    }
}
