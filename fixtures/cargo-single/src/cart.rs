//! Shopping carts.

use crate::catalog::Product;
use crate::pricing::{self, Price};

/// Products a customer intends to buy.
#[derive(Debug, Default)]
pub struct Cart {
    items: Vec<Product>,
}

impl Cart {
    /// Adds a product.
    pub fn add(&mut self, p: Product) {
        self.items.push(p);
    }

    /// Total price.
    pub fn total(&self) -> Price {
        self.items.iter().map(pricing::price).sum()
    }

    /// Total weight in grams.
    pub fn grams(&self) -> u32 {
        self.items.iter().map(|p| p.grams).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_cart_is_free() {
        assert_eq!(Cart::default().total(), 0);
    }
}
