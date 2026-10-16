# Starting the first Claude Code session

Everything Claude Code needs is in this folder: `CLAUDE.md` (conventions), `docs/design.md` (design),
`docs/backlog.md` (tasks), `docs/playground.md` (test setup) and `docs/roadmap.md` (later work).

## 1. One-time setup

```sh
cd ~/Projects/Rust/sheltie
git init -b main
git add -A
git commit -m "chore: project skeleton and design docs"
rustup show                      # installs the toolchain pinned in rust-toolchain.toml
cargo test --workspace           # should pass: 3 tests in sheltie-core
```

Optional, once the GitHub org exists (see `docs/playground.md`):

```sh
gh repo create softberries/sheltie --private --source . --push
scripts/create-issues.sh softberries/sheltie --dry-run   # preview
scripts/create-issues.sh softberries/sheltie             # create one issue per open task
```

The repository lives at `softberries/sheltie` (referenced in `Cargo.toml`, `SECURITY.md` and `.github/`);
change it there if it moves.

## 2. Kick-off prompt for M0

Start `claude` in the repository root and paste:

> Read `CLAUDE.md` and `docs/design.md`, then `docs/backlog.md`. We are starting milestone M0 on
> branch `m0-skeleton`. The workspace, CI and community files are already scaffolded; the remaining
> M0 tasks are the core domain types (`Change`, `Verification`, `Resolution`, `HeadEvent` plus state
> enums, alongside the existing `Footprint` and `Relation` in `sheltie-core`) and the Postgres schema
> with sqlx migrations in `sheltie-store`, plus a `sheltie-server` that starts, connects to Postgres
> and serves a health endpoint. Write a short plan (tasks, files, tests) and wait for my approval
> before changing code.

## 3. Rhythm for later milestones

- One milestone per branch (`m<N>-<short-name>`); plan first, then implement task by task with tests.
- Close each milestone with the integration test named in its "Done when" in `docs/design.md`.
- After M4, run the early public Cargo demo (M4.5 in `docs/backlog.md`) before moving on to M5.

## Decisions still open

- License (see "Licensing and monetization notes" in `docs/design.md`); `LICENSE` is deliberately absent.
- ~~Final name and domain~~: `sheltie.dev` registered 2026-10-16.
- SaaS versus Softberries showcase (open question in the design doc).
