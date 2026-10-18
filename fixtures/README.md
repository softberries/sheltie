# Fixtures

Sample repositories and scripted scenarios used by integration and snapshot tests.

- `cargo-single/`: one crate (`shop`) with several modules: re-exports, a type alias used in
  signatures, a glob import, a `macro_rules!`, nested modules and an integration test. Most Rust
  projects are a single crate, so module-level footprints are tested here first.
- `cargo-workspace/`: the five-crate `ledger` workspace, a copy of
  `softberries/sheltie-playground-cargo`.
- `go-workspace/`: the `example.com/shop` Go module: `pricing` read by `cart`, `invoice` and
  `cmd/shop`, an independent `search` with `testdata/`, a shared `internal/util`, and test
  files (including an external `_test` package).
- `scenarios/`: scripted changes per fixture, with the groups that must commute. The golden
  footprints are in `crates/sheltie-cargo/tests/snapshots/` and
  `crates/sheltie-go/tests/snapshots/`.

Planned for M4–M5: `Cargo.lock` and registry-file conflicts between PRs, and a seeded bad commit.

Never put secrets, customer data or private repository content here.
