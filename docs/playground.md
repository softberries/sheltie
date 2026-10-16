# Sheltie — Test playground setup

Manual setup steps before Claude Code can run end-to-end tests against real GitHub.

A separate GitHub organization keeps experiments away from real repositories and lets Sheltie hold merge rights safely.

- [x] Register `sheltie.dev`; repository at `softberries/sheltie`.
- [ ] Create the GitHub org and the main repository `sheltie`.
- [ ] Create a second org (or private repos) for testing, e.g. `sheltie-playground`.
- [ ] Register a development GitHub App: permissions for contents (write), pull requests (write), checks (write), issues (write), metadata (read); webhooks for pull_request, pull_request_review, check_suite, check_run, issues, push.
- [ ] Store the App private key and webhook secret in a password manager; expose webhooks locally with a tunnel (smee.io or cloudflared).
- [ ] Create `playground-cargo`: a small Cargo workspace (4–6 crates with real dependencies between them) and protected `main`.
- [ ] Add a script that opens N scripted PRs: independent changes, import conflicts, `Cargo.lock` conflicts, registry-file conflicts, an interface change that breaks a dependent crate.
- [ ] Add a seeded "bad commit" scenario to test sweeps, bisect and auto-revert.
- [ ] Later: `playground-sbt`, `playground-gradle`, `playground-maven`, which become the public demo repositories.
