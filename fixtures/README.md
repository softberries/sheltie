# Fixtures

Sample repositories and scripted conflict scenarios used by integration and snapshot tests.

Planned (see `docs/backlog.md`, M2 and M4):

- `cargo-sample/` — a 4–6 crate Cargo workspace with real dependencies between crates.
- `scenarios/` — scripted changes: independent edits, import conflicts, `Cargo.lock` conflicts,
  registry-file conflicts, an interface change that breaks a dependent crate, a seeded bad commit.

Never put secrets, customer data or private repository content here.
