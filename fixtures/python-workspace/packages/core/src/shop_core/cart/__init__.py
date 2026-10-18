"""A shopping cart."""

from ..pricing import Money, price


class Cart:
    """Items and their net prices."""

    def __init__(self) -> None:
        self._items: list[tuple[Money, int]] = []

    def add(self, net: Money, qty: int = 1) -> None:
        self._items.append((net, qty))

    def total(self) -> Money:
        result = Money(0)
        for net, qty in self._items:
            result = result + price(net, qty)
        return result
