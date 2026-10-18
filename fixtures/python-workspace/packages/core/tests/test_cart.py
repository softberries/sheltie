from shop_core import Cart, Money


def test_total():
    cart = Cart()
    cart.add(Money(100), 2)
    assert cart.total() == Money(240)
