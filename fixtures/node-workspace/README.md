# shop (node-workspace fixture)

A small pnpm monorepo for testing Sheltie's JavaScript/TypeScript adapter (`sheltie-node`).
It is read from git only; nothing here is installed or built.

```text
@shop/web ──► @shop/ui ──────────► @shop/utils
     └──────► @shop/api-client ──► @shop/utils
```

- `packages/utils`: price formatting and money helpers.
- `packages/ui`: React components that display prices.
- `packages/api-client`: a typed HTTP client for orders, with JSON fixtures.
- `apps/web`: the storefront; also an Nx project (`project.json`).
