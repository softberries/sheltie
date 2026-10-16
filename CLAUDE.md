# Sheltie — guide for Claude Code

Sheltie is an open-source parallel merge engine for agent-generated pull requests.
It runs as a GitHub App, computes a footprint (write set / read set) for every PR,
lands independent PRs in parallel, and resolves conflicts in escalating tiers.
The full design is in `docs/design.md`. Read it before starting any milestone.
The task list is `docs/backlog.md`; test setup is `docs/playground.md`; later work is `docs/roadmap.md`.

## Workspace layout

- `crates/sheltie-core` — domain types only. No I/O, no async, no external services.
- `crates/sheltie-graph` — build-graph snapshots (`GraphSnapshot`) and the footprint algorithm.
- `crates/sheltie-cargo` — Cargo adapter (library + `sheltie-cargo` CLI).
- `crates/sheltie-github` — GitHub App: webhooks, check runs, labels, branches, merges.
- `crates/sheltie-scheduler` — conflict graph, epochs, strategies.
- `crates/sheltie-merge` — resolution tiers 1-3 and the tier-4 replay client.
- `crates/sheltie-commit` — optimistic commit check + compare-and-swap on head.
- `crates/sheltie-sweeper` — sweeps, bisect, auto-revert.
- `crates/sheltie-store` — Postgres via sqlx; migrations in `crates/sheltie-store/migrations`.
- `crates/sheltie-api` — REST (axum) and MCP server.
- `crates/sheltie-server` — the binary; wiring and configuration only.
- `fixtures/` — sample repositories and scripted conflict scenarios for integration tests.

Dependency direction: `core` <- everything. Adapters never depend on the server.
The control plane never invokes build tools; adapters run on CI runners and upload footprints.

## Commands

- Build: `cargo build --workspace`
- Test: `cargo nextest run --workspace` (fallback: `cargo test --workspace`)
- Lint: `cargo clippy --workspace --all-targets -- -D warnings`
- Format: `cargo fmt --all`
- Dependency audit: `cargo deny check`
- Local services: `docker compose -f deploy/docker-compose.yml up -d` (Postgres; copy `.env.example` to `.env`)
- DB migrations: `sqlx migrate run --source crates/sheltie-store/migrations`
- Offline sqlx metadata after query changes: `cargo sqlx prepare --workspace`

Before saying a task is done, run fmt, clippy and tests. All must pass.

## Rust conventions

- Edition 2024. MSRV is `rust-version` in the root `Cargo.toml` (1.94, set by sqlx 0.9) and is checked in CI;
  the development toolchain is pinned in `rust-toolchain.toml`. Stable only.
- `unsafe_code = "forbid"` via `[workspace.lints]`; every crate has `[lints] workspace = true`.
  No exceptions without an issue and review.
- Errors: `thiserror` for library crates, `anyhow` only in `sheltie-server` and CLIs.
  Never `unwrap()` or `expect()` outside tests, except on invariants documented with a comment.
- Async: tokio. No blocking calls on async tasks; use `spawn_blocking` for CPU-heavy or blocking work.
- Logging: `tracing` with structured fields. Never log secrets, tokens or full diffs.
- Public items get rustdoc comments. Each crate has a crate-level doc explaining its role.
- Prefer newtypes for identifiers (`ChangeId`, `TargetId`, `TreeHash`), not bare strings.
- Keep `sheltie-core` pure and deterministic so it can be property-tested.
- New dependencies: justify in the PR description; prefer well-maintained crates; must pass `cargo deny`.
- Shared dependency versions live in `[workspace.dependencies]`.

## Testing

- Every task adds or updates tests. No untested logic in `core`, `graph`, `scheduler`, `commit`.
- Unit tests next to the code; integration tests in each crate's `tests/`.
- Property tests (`proptest`) for the footprint algorithm, conflict relation and commit check.
- Snapshot tests (`insta`) for footprints computed from `fixtures/`.
- GitHub interactions are tested against recorded fixtures or a fake; never hit real GitHub in CI.
- Postgres tests use a throwaway database (`sqlx::test`).
- Each milestone ends with the integration test named in its "Done when" in `docs/design.md`.

## Workflow

- Work one milestone per branch: `m<N>-<short-name>`.
- Start a milestone by writing a short plan (tasks, files, tests) and wait for approval.
- Small, focused commits using Conventional Commits (`feat:`, `fix:`, `refactor:`, `test:`, `docs:`, `chore:`).
- Update `CHANGELOG.md` (Keep a Changelog format) under `Unreleased` for user-visible changes.
- If the design needs to change, propose an edit to `docs/design.md` rather than diverging silently.
- Ask before: adding a crate to the workspace, changing the database schema, changing a public API,
  or touching the security-sensitive code paths listed below.

## Security-sensitive areas

- Webhook signature verification, GitHub App private key handling and OIDC token verification for
  footprint uploads (`sheltie-github`, `sheltie-api`).
- The commit path that pushes to protected branches (`sheltie-commit`).
- Agent replay jobs: they run with user-provided API keys; keys come only from runner secrets,
  never stored in the database or logs.
- Parsing of `.sheltie/merge.toml` and `intent.json` from untrusted PRs: validate strictly, no code execution.

## Open source hygiene

- License headers are not used; the license will be in `LICENSE` (to be decided, see `docs/design.md`).
  Until then the workspace is `publish = false`.
- Keep `README.md`, `CONTRIBUTING.md`, `SECURITY.md` and `CODE_OF_CONDUCT.md` current.
- Docs for users live in `docs/`; examples must compile (`cargo test --doc`).
- Never commit secrets, real customer data, or private repository content to `fixtures/`.
