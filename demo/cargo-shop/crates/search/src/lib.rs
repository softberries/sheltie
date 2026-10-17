//! Product search.

use shop_catalog::{Product, products};

/// Products whose name contains `query`, ignoring case.
pub fn search(query: &str) -> Vec<Product> {
    let q = query.to_lowercase();
    products().into_iter().filter(|p| p.name.to_lowercase().contains(&q)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_mugs() {
        assert_eq!(search("MUG").len(), 1);
        assert!(search("nothing").is_empty());
    }
}
