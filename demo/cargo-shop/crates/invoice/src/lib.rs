//! Invoices rendered from carts.

use shop_cart::Cart;

/// A plain-text invoice for `cart`.
pub fn render(customer: &str, cart: &Cart) -> String {
    let net = cart.net();
    format!(
        "Invoice for {customer}\nNet: {}\nVAT ({}%): {}\nTotal: {}\n",
        cents(net),
        shop_pricing::rate(),
        cents(shop_pricing::tax(net)),
        cents(cart.gross())
    )
}

fn cents(amount: u64) -> String {
    format!("{}.{:02}", amount / 100, amount % 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_totals() {
        let mut cart = Cart::default();
        cart.add("TEE-01", 1);
        let text = render("Ada", &cart);
        assert!(text.contains("Total: 30.00"), "{text}");
    }
}
