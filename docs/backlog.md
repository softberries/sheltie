# Sheltie — Issue backlog

Milestone details and "done when" criteria are in `docs/design.md` (Implementation plan). Create the GitHub issues with `scripts/create-issues.sh`.

One GitHub issue per task, labelled with its milestone; Claude Code can create these from this list once the repository exists.

**M0 — Skeleton**

- [x] Workspace with all crates, `rust-toolchain.toml`, workspace dependencies *(scaffolded)*
- [x] CI: fmt, clippy, nextest, cargo-deny, MSRV *(scaffolded)*
- [x] Core domain types with newtype IDs
- [x] Postgres schema + migrations; docker-compose
- [x] `sheltie-server` starts, migrates and serves `GET /healthz`
- [x] README, CONTRIBUTING, SECURITY, CODE_OF_CONDUCT, CHANGELOG, templates *(scaffolded)*

**M1 — GitHub App intake**

- [ ] Webhook receiver with signature verification
- [ ] Persist PR lifecycle events
- [ ] Sheltie check run per PR with state
- [ ] Installation token management

**M2 — Footprints (Cargo)**

M2 priority — module-level footprints for single-crate projects. Most Rust projects are a single crate. With crate-level targets, every PR writes the same target, so all PRs conflict and Sheltie falls back to batching and speculation. Resolve this at the start of M2, not later:

- [ ] Decide footprint granularity inside a crate: modules / files as targets (e.g. via rust-analyzer's module graph or rustdoc JSON per module)
- [ ] Interface hashing per module (public and `pub(crate)` items), so a change inside a module with an unchanged surface doesn't affect other modules
- [ ] Map tests to modules (unit tests in-module; integration tests as readers of the public API)
- [ ] Fixture: a single-crate project with several modules and scripted PRs, proving independent module changes land in parallel

- [ ] `BuildGraph` trait
- [ ] Cargo adapter: graph from `cargo metadata`
- [ ] Interface hashing via `cargo-public-api`
- [ ] Footprint algorithm + cache + `leaky` flag
- [ ] `sheltie-cargo` CLI and upload endpoint
- [ ] Snapshot tests on fixtures

**M3 — Shadow mode**

- [ ] Conflict graph and epochs
- [ ] Predicted batches written to check runs
- [ ] Prediction vs. reality log and footprint-miss report

**M4 — Parallel commit**

- [ ] Verification via GitHub Actions on temporary branches
- [ ] OCC commit check + CAS on head
- [ ] Commit trailers with guarantee tier
- [ ] Sweeper, bisect, auto-revert PR

**M4.5 — Early public Cargo demo (priority: as soon as M4 works)**

Pulled forward from M8: show Sheltie working in public as soon as the Cargo adapter and parallel commit run end to end, with no need to wait for merge tiers or intent replay.

- [ ] Make `playground-cargo` public as `sheltie-demo-cargo`
- [ ] Script generating a batch of agent-style PRs: mostly independent, some conflicting
- [ ] Sheltie lands independent PRs in parallel and serializes conflicting ones, visible in check runs
- [ ] README with live numbers: PRs landed, wall-clock time vs. a serial queue, CI runs saved
- [ ] Short screen recording / GIF for the README and social posts
- [ ] Re-run the demo after M5 and M6 to show conflicts being auto-resolved

**M5 — Merge tiers 1–3 and speculation**

- [ ] mergiraf integration
- [ ] `.sheltie/merge.toml`: derived, set, sequence
- [ ] Speculative chains
- [ ] Group-tested batches

**M6 — Intent replay**

- [ ] `intent.json` schema and validation
- [ ] Replay workflow running Claude Code headless
- [ ] Acceptance, diff-scope and similarity checks
- [ ] Re-approval labels; budgets and per-PR caps

**M7 — Coordination**

- [ ] Issue footprint prediction
- [ ] Leases with TTL
- [ ] MCP server: `safe_issues`, `claim_issue`, `predict_footprint`, `release`
- [ ] Labels and comments for non-MCP agents

**M8 — Release**

- [ ] Helm chart and Docker image
- [ ] Docs site
- [ ] Opt-in anonymous telemetry
- [ ] Public demo repositories and benchmark run
- [ ] License decision and `LICENSE`
