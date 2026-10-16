use shop::Cart;
use shop::catalog::find;
use shop::shipping::rates::rate;

#[test]
fn checkout_total_with_shipping() {
    let mut cart = Cart::default();
    cart.add(find(1).unwrap());
    assert_eq!(cart.total() + rate(cart.grams()), 125 + 300);
}
