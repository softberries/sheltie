# Sheltie — Test playground setup

Manual setup steps before Claude Code can run end-to-end tests against real GitHub.

A separate GitHub organization keeps experiments away from real repositories and lets Sheltie hold merge rights safely.

- [x] Register `sheltie.dev`; repository at `softberries/sheltie`.
- [ ] Create the GitHub org and the main repository `sheltie`.
- [ ] Create a second org (or private repos) for testing, e.g. `sheltie-playground`.
- [ ] Register a development GitHub App with `scripts/register-github-app.py` (see below). It requests only what M1 needs; later milestones widen the manifest (contents write and push events in M4, issues in M7).
- [ ] Store the App private key and webhook secret in a password manager; webhooks reach the local server through the smee.io channel the script creates.
- [x] Create `playground-cargo`: private repo `softberries/sheltie-playground-cargo`, a 5-crate `ledger` workspace with real dependencies and a CI workflow (`test`).
- [ ] Protect its `main` (require the `test` check, no force pushes). Branch protection on a private repo needs GitHub Pro; alternatively make the repo public, which M4.5 plans anyway. Needed before M4.
- [ ] Add a script that opens N scripted PRs: independent changes, import conflicts, `Cargo.lock` conflicts, registry-file conflicts, an interface change that breaks a dependent crate.
- [ ] Add a seeded "bad commit" scenario to test sweeps, bisect and auto-revert.
- [ ] Later: `playground-sbt`, `playground-gradle`, `playground-maven`, which become the public demo repositories.

## Running the GitHub App locally (M1)

1. Start Postgres and copy the env file: `docker compose -f deploy/docker-compose.yml up -d`, `cp .env.example .env`.
2. Register the App: `scripts/register-github-app.py` (add `--org <org>` to register it under an organization,
   `--name` if "Sheltie (dev)" is taken). Confirm the App in the browser. The script writes
   `sheltie-github-app.pem` (git-ignored) and adds the three `SHELTIE_GITHUB_*` settings to `.env`.
   It creates a smee.io channel for webhooks, unless you pass `--webhook-url`.
3. Forward webhooks, then run the server, each in its own terminal:
   `npx smee-client --url <channel> --target http://127.0.0.1:8080/webhooks/github` and
   `cargo run -p sheltie-server`.
4. Install the App on a playground repository from the App's settings page, then open a pull request.
   The PR shows a **Sheltie** check, queued with the title "Waiting to be ready".

Permissions in `deploy/github-app-manifest.json`: checks (write), contents (read), metadata (read),
pull requests (read); events: `pull_request`, `pull_request_review`. GitHub Apps always receive
`installation` and `installation_repositories` events.
