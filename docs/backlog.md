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

- [x] Webhook receiver with signature verification
- [x] Persist PR lifecycle events
- [x] Sheltie check run per PR with state
- [x] Installation token management
- [x] Register the development App and see the check on a playground PR (`sheltie-dev`, playground PR #2, 2026-10-17)

**M2 — Footprints (Cargo)**

M2 priority — module-level footprints for single-crate projects. Most Rust projects are a single crate. With crate-level targets, every PR writes the same target, so all PRs conflict and Sheltie falls back to batching and speculation. Resolve this at the start of M2, not later:

- [x] Decide footprint granularity inside a crate: modules as targets, parsed with `syn` (rustdoc JSON is nightly-only)
- [x] Interface hashing per module, so a change inside a module with an unchanged surface doesn't affect other modules
- [x] Map tests to modules (unit tests in-module; integration tests as readers of the public API)
- [x] Fixture: a single-crate project with several modules and scripted PRs, proving independent module changes commute

- [x] `BuildGraph` as a data snapshot (`GraphSnapshot`) plus a pure footprint function
- [x] Cargo adapter: graph from `cargo metadata`
- [x] Interface hashing (syn-based; replaces `cargo-public-api`, see design doc)
- [x] Footprint algorithm + `leaky` flag
- [x] Footprints stored by repository and head, indexed by `(base_tree, diff_hash)` for reuse
- [x] `sheltie-cargo` CLI (`footprint`, `graph`)
- [x] Upload endpoint (`POST /v1/footprints`) authenticated with GitHub Actions OIDC
- [x] Reusable workflow `.github/workflows/footprint.yml`
- [ ] Run the workflow on the playground against a reachable Sheltie (needs a deployment or tunnel)
- [ ] Verify whether runs for PRs from forks can obtain OIDC tokens (see design doc, Upload)
- [x] Snapshot tests on fixtures

**M3 — Shadow mode**

- [x] Readiness from GitHub (approval, checks, draft) with event-driven and periodic rechecks
- [x] Default-branch history from `push` events; merged PRs' merge commits
- [x] Conflict graph and epochs (`sheltie-scheduler`, property-tested)
- [x] Predicted batches written to check runs
- [x] Prediction vs. reality log and footprint-miss report (`sheltie-server shadow-report`)
- [ ] A week of shadow data on a real repository (needs a deployment; see #51)

**M4 — Parallel commit**

- [x] Verification via the repository's CI on `sheltie/verify/*` branches
- [x] OCC commit check + CAS on head (`force=false` fast-forward; 422 on a moved head)
- [x] Commit trailers with guarantee tier
- [x] Per-repository merge mode (`sheltie-server repo set-mode`)
- [x] Sweeps and the verified prefix (`sheltie/verified`), bisect via `sheltie/bisect/*`, auto-revert PR landed by Sheltie
- [ ] Flaky-test quarantine (a lone red tip is not reverted until then)

**M4.5 — Early public Cargo demo (priority: as soon as M4 works)**

Pulled forward from M8: show Sheltie working in public as soon as the Cargo adapter and parallel commit run end to end, with no need to wait for merge tiers or intent replay.

- [ ] Make `playground-cargo` public as `sheltie-demo-cargo`
- [ ] Script generating a batch of agent-style PRs: mostly independent, some conflicting
- [ ] Sheltie lands independent PRs in parallel and serializes conflicting ones, visible in check runs
- [ ] README with live numbers: PRs landed, wall-clock time vs. a serial queue, CI runs saved
- [ ] Short screen recording / GIF for the README and social posts
- [ ] Re-run the demo after M5 and M6 to show conflicts being auto-resolved

**M5 — Merge tiers 1–3 and speculation**

- [x] mergiraf integration (tier 1, run as a separate program on the runner)
- [x] `.sheltie/merge.toml`: derived, set, sequence
- [x] Merge engine on the runner: `repository_dispatch` → `.github/workflows/merge.yml` → `POST /v1/merges`
- [ ] Run the merge workflow on the playground (needs the public deployment, #51)
- [x] Speculative chains (every prefix of groups up to 8)
- [x] Group-tested batches (prefixes 1, 2, 4, …, n with bisection)

**M6 — Intent replay**

- [x] Intent schema and validation (`sheltie-intent` block in the PR description)
- [x] Replay workflow running Claude Code headless (`.github/workflows/replay.yml`)
- [x] Acceptance, diff-scope and similarity checks
- [x] Re-approval labels; budgets and per-PR caps
- [ ] Run a replay with real Claude Code on the playground (needs the public deployment, #51, and an API key secret)

**M7 — Coordination**

- [x] Issue footprint prediction (default-branch graph uploaded on push; matched against issue text)
- [x] Leases with TTL
- [x] MCP server: `safe_issues`, `claim_issue`, `predict_footprint`, `release`
- [x] Labels and comments for non-MCP agents (assignment claims; `sheltie: blocked`)
- [ ] Accept the Issues permission on the dev App installation and try it on the playground
- [ ] Fairness lease: a PR that lost three epochs reserves its read set (scheduler, `docs/design.md`)

**M9 — Polish, website and demo**

- [x] Redesigned check run, one voice for comments, labels with colors, terminal shadow report
- [x] Logo, sheltie.dev landing page, branded docs, social image, Pages workflow
- [x] Read-only dashboard with GitHub sign-in
- [ ] Public demo instance on a VM (compose + Caddy) at a sheltie.dev subdomain
- [x] Demo project and PR generator (`demo/cargo-shop`), scenarios checked with `sheltie-cargo`
- [ ] Demo repository from it, recorded run (GIF/video) and published benchmark
- [ ] Replace illustrative screenshots with real ones from the demo
- [ ] Go public: make the repository public, enable Pages (`PAGES_ENABLED`), dry-run and tag v0.1.0

**M8 — Release**

- [x] Helm chart and Docker image (`deploy/`, `compose.yaml`; chart tested on kind in CI)
- [x] Docs site (`mdbook build docs`; publish to GitHub Pages once the repository is public)
- [x] Telemetry: deferred (no endpoint to run); `sheltie-server stats` prints anonymous counts instead
- [ ] Public demo repositories and benchmark run (deferred: "demo later")
- [x] License decision and `LICENSE` (Apache-2.0)
- [x] Release workflow (binaries, SHA256SUMS, GHCR image); tag `v0.1.0` when the repository is public
- [ ] A self-host walkthrough by someone new to the project (timed, from the README)
