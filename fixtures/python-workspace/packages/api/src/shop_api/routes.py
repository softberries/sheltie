"""Request handlers."""

from shop_core.pricing import Money, price

from .auth import require_scope


def checkout(token: str, net_cents: int, qty: int = 1) -> dict[str, int]:
    require_scope(token, "checkout")
    gross = price(Money(net_cents), qty)
    return {"total": gross.cents}
