# sheltie-playground-cargo

A small Cargo workspace for testing [Sheltie](https://github.com/softberries/sheltie) end to end.
PRs against this repository are scripted: independent edits, import conflicts, `Cargo.lock`
conflicts, registry-file conflicts and interface changes that break a dependent crate.

```text
ledger-api ──► ledger-billing ──► ledger-store ──► ledger-core
     └───────► ledger-auth ─────────────────────► ledger-core
```

- `ledger-core`: money and account types shared by everything.
- `ledger-store`: account storage behind a trait, with an in-memory implementation.
- `ledger-auth`: API tokens and scopes.
- `ledger-billing`: invoices, tax and discounts, in separate modules.
- `ledger-api`: request handlers that check auth and call billing.
- `config/feature_flags.txt`: an order-insensitive registry file (a merge-rule test case).

`cargo test --workspace` must pass on `main`.
