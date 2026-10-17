//! Shopping carts: lines of products and their totals.

use shop_catalog::find;

/// A cart: (SKU, quantity) lines.
#[derive(Debug, Default, Clone)]
pub struct Cart {
    lines: Vec<(String, u32)>,
}

impl Cart {
    /// Adds `qty` of `sku`; unknown SKUs are ignored.
    pub fn add(&mut self, sku: &str, qty: u32) {
        if find(sku).is_some() {
            self.lines.push((sku.to_owned(), qty));
        }
    }

    /// Net total in cents.
    pub fn net(&self) -> u64 {
        self.lines
            .iter()
            .filter_map(|(sku, qty)| find(sku).map(|p| p.price * u64::from(*qty)))
            .sum()
    }

    /// Total including VAT, in cents.
    pub fn gross(&self) -> u64 {
        let net = self.net();
        net + shop_pricing::tax(net)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals() {
        let mut cart = Cart::default();
        cart.add("MUG-01", 2);
        cart.add("STK-01", 1);
        cart.add("NOPE", 9);
        assert_eq!(cart.net(), 2_800);
        assert_eq!(cart.gross(), 3_360);
    }
}
