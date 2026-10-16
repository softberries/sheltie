//! Discounts.

use ledger_core::Money;

/// Orders at or above this net amount get the volume discount.
pub const VOLUME_THRESHOLD: Money = Money(100_000);

/// Volume discount in percent.
pub const VOLUME_PCT: i64 = 5;

/// Discount on a net amount.
pub fn discount_on(net: Money) -> Money {
    if net >= VOLUME_THRESHOLD { net.percent(VOLUME_PCT) } else { Money::ZERO }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_large_orders_get_a_discount() {
        assert_eq!(discount_on(Money(99_999)), Money::ZERO);
        assert_eq!(discount_on(Money(100_000)), Money(5_000));
    }
}
