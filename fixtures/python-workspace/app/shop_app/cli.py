"""Command line entry point."""

import sys

import shop_api


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    net = int(args[0])
    qty = int(args[1]) if len(args) > 1 else 1
    print(shop_api.checkout("cli:checkout", net, qty)["total"])
    return 0
