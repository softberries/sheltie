from shop_core.pricing import Money, price


def test_price_adds_tax():
    assert price(Money(100)) == Money(120)


def test_quantity():
    assert price(Money(100), qty=2) == Money(240)
