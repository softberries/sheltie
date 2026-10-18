"""Prices with tax."""

from __future__ import annotations

from dataclasses import dataclass

TAX_RATE = 20


@dataclass(frozen=True)
class Money:
    """An amount in cents."""

    cents: int
    currency: str = "EUR"

    def __add__(self, other: Money) -> Money:
        return Money(self.cents + other.cents, self.currency)

    def scaled(self, pct: int) -> Money:
        return Money(_round(self.cents * pct / 100), self.currency)


def price(net: Money, qty: int = 1) -> Money:
    """The gross price of `qty` items."""
    total = Money(net.cents * qty, net.currency)
    return total + total.scaled(TAX_RATE)


def _round(value: float) -> int:
    return int(value + 0.5)
