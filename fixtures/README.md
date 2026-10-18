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
- `node-workspace/`: the four-package `shop` pnpm monorepo (`@shop/utils`, `@shop/ui`,
  `@shop/api-client` and the `apps/web` Nx project), for `sheltie-node`.
- `python-workspace/`: a uv workspace (`shop_core` with a `shop_core.cart` subpackage,
  `shop_api`, the `shop_app` command line) with tests per project, for `sheltie-python`.
- `maven-multimodule/`: the `shop-parent` Maven build: `core` (Java), `api` (depends on
  `core`) and `app` (depends on `api`), with tests, resources and `.mvn/maven.config`.
- `gradle-multiproject/`: the same shop as a Gradle build (Kotlin DSL): `:core` (Kotlin),
  `:api` (Java, depends on `:core` through `projects.core`), `:app` (Java, depends on
  `project(":api")`) and a version catalog.
- `scenarios/`: scripted changes per fixture, with the groups that must commute. The golden
  footprints are in each adapter's `tests/snapshots/` (`crates/sheltie-<tool>`).

Planned for M4–M5: `Cargo.lock` and registry-file conflicts between PRs, and a seeded bad commit.

Never put secrets, customer data or private repository content here.
