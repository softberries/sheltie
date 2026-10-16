//! Prices.

use crate::catalog::Product;
use crate::util::*;

/// A price in cents.
pub type Price = u64;

/// The price of one product, VAT included.
pub fn price(p: &Product) -> Price {
    round5(p.cents * 120 / 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::find;

    #[test]
    fn includes_vat() {
        assert_eq!(price(&find(0).unwrap()), 120);
    }
}
