//! Products.

/// A product for sale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Product {
    pub sku: String,
    pub cents: u64,
    pub grams: u32,
}

/// Looks up a product by number.
pub fn find(n: u32) -> Option<Product> {
    (n < 100).then(|| Product {
        sku: sku!("gen", n),
        cents: 100 + u64::from(n),
        grams: 50 * n,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_small_numbers() {
        assert_eq!(find(1).map(|p| p.sku), Some("gen-0001".into()));
        assert!(find(100).is_none());
    }
}
