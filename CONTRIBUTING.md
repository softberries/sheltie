# Contributing to Sheltie

Thanks for your interest. Sheltie is early (0.x); the best place to start is the
[documentation](https://sheltie.dev/docs/) and the open issues.

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

## Running the GitHub App locally

1. Start Postgres and copy the env file as above.
2. Register a development App: `scripts/register-github-app.py` (add `--org <org>` to register it
   under an organization, `--name` if "Sheltie (dev)" is taken) and confirm it in the browser. The
   script writes `sheltie-github-app.pem` (git-ignored), adds the `SHELTIE_GITHUB_*` settings to
   `.env` and creates a smee.io channel for webhooks, unless you pass `--webhook-url`.
3. In two terminals, forward webhooks and run the server:
   `npx smee-client --url <channel> --target http://127.0.0.1:8080/webhooks/github` and
   `cargo run -p sheltie-server`.
4. Install the App on a test repository from the App's settings page and open a pull request. It
   shows a **Sheltie** check, queued with the title "Waiting to be ready".

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
- **Design changes:** describe the change and its reasoning in the issue or PR description.

## AI-assisted contributions

Contributions written with coding agents are welcome; this project exists because of them. You are
responsible for what you submit: review it, make sure it is tested, and follow the guidelines above.

## License of contributions

Sheltie is licensed under the Apache License 2.0 (`LICENSE`). Unless you state otherwise, any
contribution you submit is licensed under the same terms, as section 5 of the license says;
there is no separate contributor agreement.
