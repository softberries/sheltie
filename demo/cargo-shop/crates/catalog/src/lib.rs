//! The product catalog.

/// A product for sale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Product {
    /// Stock-keeping unit.
    pub sku: &'static str,
    /// Display name.
    pub name: &'static str,
    /// Net price in cents.
    pub price: u64,
}

/// Everything the shop sells.
pub fn products() -> Vec<Product> {
    vec![
        Product { sku: "MUG-01", name: "Sheltie mug", price: 1_200 },
        Product { sku: "TEE-01", name: "Merge queue T-shirt", price: 2_500 },
        Product { sku: "STK-01", name: "Sticker pack", price: 400 },
    ]
}

/// The product with this SKU.
pub fn find(sku: &str) -> Option<Product> {
    products().into_iter().find(|p| p.sku == sku)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_by_sku() {
        assert_eq!(find("MUG-01").map(|p| p.price), Some(1_200));
        assert_eq!(find("NOPE"), None);
    }
}
