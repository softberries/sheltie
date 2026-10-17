# Contributing to Sheltie

Thanks for your interest. Sheltie is early (0.x); the best place to start is
[`docs/design.md`](docs/design.md) and the open issues for the current milestone.

## Before you start

- For anything bigger than a small fix, open or comment on an issue first so we can agree on the approach.
- Security issues: do **not** open a public issue. See [`SECURITY.md`](SECURITY.md).

## Development setup

```sh
rustup show                                          # installs the pinned toolchain
cargo install cargo-nextest cargo-deny sqlx-cli mergiraf mdbook --locked   # mergiraf: merge-engine tests
cp .env.example .env                                 # DATABASE_URL is also used by the DB tests
docker compose -f deploy/docker-compose.yml up -d
cargo build --workspace
```

## Before opening a PR

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace      # or: cargo test --workspace
cargo deny check
```

All four must pass; CI runs the same checks plus an MSRV build. If you touch `deploy/`, also run
`scripts/test-helm-kind.sh` (needs kind and helm); if you touch `docs/`, `mdbook build docs`.

## Guidelines

- **Tests are required** for new logic. Pure logic in `sheltie-core`, `-graph`, `-scheduler` and
  `-commit` gets unit and property tests.
- **No `unsafe`.** It is forbidden workspace-wide.
- **Errors:** `thiserror` in libraries, `anyhow` only in binaries. No `unwrap()`/`expect()` outside tests.
- **Dependencies:** add them to `[workspace.dependencies]` and explain why in the PR description.
- **Commits:** [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`, `docs:` …).
- **Changelog:** add user-visible changes to `CHANGELOG.md` under `Unreleased`.
- **Design changes:** update `docs/design.md` in the same PR.

## AI-assisted contributions

Contributions written with coding agents are welcome; this project exists because of them. You are
responsible for what you submit: review it, make sure it is tested, and follow [`CLAUDE.md`](CLAUDE.md).

## License of contributions

Sheltie is licensed under the Apache License 2.0 (`LICENSE`). Unless you state otherwise, any
contribution you submit is licensed under the same terms, as section 5 of the license says;
there is no separate contributor agreement.
