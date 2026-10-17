# Sheltie — Test playground setup

Manual setup steps before Claude Code can run end-to-end tests against real GitHub.

A separate GitHub organization keeps experiments away from real repositories and lets Sheltie hold merge rights safely.

- [x] Register `sheltie.dev`; repository at `softberries/sheltie`.
- [ ] Create the GitHub org and the main repository `sheltie`.
- [ ] Create a second org (or private repos) for testing, e.g. `sheltie-playground`.
- [x] Register a development GitHub App with `scripts/register-github-app.py` (see below): `sheltie-dev`, installed on the playground. It requests only what M1 needs; later milestones widen the manifest (contents write and push events in M4, issues in M7).
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
pull requests (read), commit statuses (read); events: `pull_request`, `pull_request_review`,
`check_suite`, `check_run`, `status`, `push`. GitHub Apps always receive `installation` and
`installation_repositories` events. An App registered before M3 needs the statuses permission and
the four new events added on its settings page; the installation then asks to approve them.

## Shadow mode (M3)

With the App installed and the footprint workflow running on the playground, merge PRs as usual
(by hand or through GitHub's merge queue). Each PR's Sheltie check shows what Sheltie would have
done. After a few days:

```sh
cargo run -p sheltie-server -- shadow-report --repo softberries/sheltie-playground-cargo --days 7
```

To produce a suspected miss on purpose, merge two PRs that change different modules, each green
on its own, that break `main` together. For example, one changes `ledger_billing::tax::tax_on`'s
body (a body-only change, so its footprint does not reach `invoice`) while the other adds an
`invoice` test that pins the old tax result. Once `main` is red, the report lists the pair.

## Merge mode (M4)

Prerequisites, in this order:

1. Resolve #52 (can fork PRs mint OIDC tokens?); merge mode trusts uploaded footprints.
2. Give the App contents (write) and pull requests (write) and approve them on the installation.
3. Protect the default branch with a ruleset that lists the App in its bypass list (rulesets on
   private repositories need GitHub Team or Enterprise; the playground can instead be made public).
4. Make CI run on pushes to `sheltie/verify/**` (most workflows run on every push already).

Then switch the playground:

```sh
cargo run -p sheltie-server -- repo set-mode --repo softberries/sheltie-playground-cargo --mode merge
```

Sheltie now lands ready PRs itself; switch back with `--mode shadow`.
