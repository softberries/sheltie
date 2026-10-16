/// Builds a SKU string from parts.
macro_rules! sku {
    ($cat:expr, $n:expr) => {
        format!("{}-{:04}", $cat, $n)
    };
}
