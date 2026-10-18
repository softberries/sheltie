# shop (Python fixture)

A small uv workspace for testing Sheltie's Python adapter.

```text
shop_app ──► shop_api ──► shop_core ◄── shop_core.cart
```

- `packages/core`: `shop_core` (money and prices) and its `shop_core.cart` subpackage.
- `packages/api`: `shop_api`, request handlers using `shop_core.pricing`.
- `app`: `shop_app`, the command line, using `shop_api`.

`uv run --with pytest pytest` must pass.
