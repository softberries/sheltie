# Footprint scenarios

Each `<fixture>.toml` lists scripted changes to a fixture in `fixtures/`. The test harness
of the fixture's adapter (`crates/sheltie-cargo/tests/scenarios.rs` for `cargo-*`,
`crates/sheltie-go/tests/scenarios.rs` for `go-*`) commits the fixture as the base, applies
one scenario as the head commit, computes the footprint and compares it with the snapshot in
that crate's `tests/snapshots/`.
(`crates/sheltie-cargo/tests/scenarios.rs`, or `crates/sheltie-node/tests/scenarios.rs` for
`node-workspace`) commits the fixture as the base, applies one
scenario as the head commit, computes the footprint and compares it with the snapshot in that
crate's `tests/snapshots/`.

A scenario has a `name`, a `description` and any number of edits:

- `[[scenarios.replace]]` with `path`, `find` and `replace` (`find` must occur exactly once);
- `[[scenarios.write]]` with `path` and `content` (creates or overwrites a file).

`independent` lists groups of scenarios whose footprints must be pairwise `Independent`.
