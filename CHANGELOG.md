# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Project skeleton: Cargo workspace with crate stubs, CI, lint and dependency policy, community files.
- Design document, issue backlog, playground setup and post-v1 roadmap in `docs/`.
- `sheltie-core`: identifier newtypes, `Footprint` and the conflict `Relation`.
- `sheltie-core`: `Change` with its `ChangeState` lifecycle, `Verification`, `Resolution` with
  `ResolutionTier`, `HeadEvent` with `GuaranteeTier`, validated git object ids and `Score`.
- `sheltie-store`: initial Postgres schema (changes, footprints, verifications, resolutions,
  head events) with embedded sqlx migrations.
- `sheltie-server`: starts from `DATABASE_URL` / `SHELTIE_BIND`, applies migrations on startup,
  serves `GET /healthz`, and shuts down gracefully on Ctrl-C or SIGTERM.

- GitHub App intake: `POST /webhooks/github` verifies `X-Hub-Signature-256`, parses
  `pull_request`, `pull_request_review`, `installation` and `installation_repositories` events,
  and stores them idempotently per delivery. Out-of-order deliveries cannot resurrect an old head.
- A "Sheltie" check run on every pull request head, kept in sync with the change's state by a
  background worker (outbox with leases and exponential backoff).
- Configuration: `SHELTIE_GITHUB_APP_ID`, `SHELTIE_GITHUB_PRIVATE_KEY_PATH`,
  `SHELTIE_GITHUB_WEBHOOK_SECRET` (all or none) and optional `SHELTIE_GITHUB_API_URL`.
- `deploy/github-app-manifest.json` and `scripts/register-github-app.py` to register a
  development App with least-privilege permissions in one step.

- Footprints (M2): `sheltie-graph` computes write and read sets from two build-graph snapshots;
  `sheltie-cargo footprint --base <rev>` computes module-level footprints for Cargo
  workspaces from git, with no build and no nightly toolchain. `sheltie-cargo graph` prints the
  graph Sheltie sees. Golden tests cover a single-crate and a multi-crate fixture.

- Footprint upload: `POST /v1/footprints` accepts reports from CI authenticated by GitHub
  Actions OIDC tokens (`SHELTIE_OIDC_AUDIENCE`, default `sheltie`; `SHELTIE_OIDC_ISSUER`).
  The "Sheltie" check run shows each change's footprint. Reusable workflow
  `.github/workflows/footprint.yml` builds `sheltie-cargo`, computes and uploads it.

- Shadow mode (M3): changes become ready when GitHub reports them approved and green and their
  footprint is known; every `SHELTIE_EPOCH_SECONDS` (default 10) the scheduler plans the ready
  changes and each Sheltie check shows what Sheltie would do ("would land now, together with
  #12"). Nothing is merged. The App now also needs commit statuses (read) and the `check_suite`,
  `check_run`, `status` and `push` events.

- Shadow report: Sheltie tracks CI on the default branch, records suspected footprint misses
  (independent pairs that landed before a red commit) and `sheltie-server shadow-report --repo
  owner/name` prints predictions versus reality.

- Merge mode (M4): in repositories switched with `sheltie-server repo set-mode --mode merge`,
  Sheltie lands ready changes itself. Independent changes land at once as merge commits through
  a compare-and-swap on the default branch (`Sheltie-Verified: footprint`); others are merged onto
  the head on `sheltie/verify/*` branches, checked by the repository's CI and then landed
  (`Sheltie-Verified: speculation`); changes that cannot land are returned with a comment. The App
  now needs contents (write) and pull requests (write).

- Sweeps (M4): Sheltie keeps `sheltie/verified` at the newest green commit of the default branch
  (the deploy gate). When the branch turns red and stays red, it bisects the commits since the
  last green one, opens a revert PR for the culprit and lands it, comments on the culprit's PR,
  and marks footprint misses involving a footprint-proven culprit as confirmed.

- Speculation (M5): changes with semantic conflicts are built together as prefixes on
  `sheltie/spec/*` branches; the longest green prefix lands in one fast-forward and the change
  that broke the build is returned with a comment naming what it was combined with.

- Merge engine (M5): `sheltie-merge` resolves conflicts on the repository's runner with
  mergiraf (tier 1), regenerated derived files such as `Cargo.lock` (tier 2) and declared rules in
  `.sheltie/merge.toml` (tier 3: set union, sequence renumbering). Sheltie triggers it with
  `repository_dispatch`, verifies the reported merge commit and lands it. Enable per repository
  with `sheltie-server repo set-merge-engine --state on` after adding the workflow.

- Intent replay (tier 4): a `sheltie-intent` block in the PR description lets Sheltie re-derive a
  change with Claude Code headless when the merge engine cannot resolve it (4a) or it breaks CI
  once merged (4b). Configured by `[replay]` in `.sheltie/merge.toml` (off by default), run on
  your runner by the reusable `.github/workflows/replay.yml` with your own API key, checked for
  scope, acceptance tests and similarity, reported to `POST /v1/replays`, capped per PR and per
  repository per day, and labelled `sheltie: replayed` or `sheltie: needs re-approval`.

- Agent coordination: Sheltie predicts issue footprints from the default branch's build graph
  (uploaded by `footprint.yml` on pushes), and serializes agents on overlapping issues with
  leases. Agents use an MCP server at `/mcp` (`safe_issues`, `claim_issue`,
  `predict_footprint`, `release`) or REST with tokens from `sheltie-server token create`;
  agents assigned on GitHub are claimed by assignment, with a `sheltie: blocked` label and
  comments while they wait. See `docs/agents.md`. The App now needs Issues write and the
  `issues` event.

- Release packaging:
  - container image (`deploy/Dockerfile`, distroless, non-root, built-in health check);
  - self-host stack (`compose.yaml`) and `scripts/register-github-app.py --url` for production
    Apps;
  - Helm chart (`deploy/helm/sheltie`, tested on kind in CI);
  - release workflow producing binaries for Linux x86_64/arm64 and macOS arm64 with
    `SHA256SUMS`, and a multi-arch image on GHCR.
- The reusable workflows take `sheltie-version` to download released, checksum-verified
  binaries instead of building from source.
- Documentation site (`mdbook build docs`): self-hosting, configuration, workflows, merge rules,
  intent replay and agents.
- `sheltie-server stats` prints anonymous usage counts to share by hand (Sheltie sends no
  telemetry); `sheltie-server healthcheck` for container health checks.

- A read-only dashboard at `/dashboard`: sign in with GitHub to see, per repository you can
  access, the queue, recent landings and how they were proven, returned pull requests and
  agents' claimed issues. Set `SHELTIE_PUBLIC_URL`, `SHELTIE_GITHUB_CLIENT_ID` and
  `SHELTIE_GITHUB_CLIENT_SECRET` (the registration script does).
- sheltie.dev: landing page, branded docs and a Pages workflow (`scripts/build-site.sh`).
- A logo.
- Sheltie's labels are created with colors and descriptions.
- `shadow-report --format text|markdown|json`, with a readable terminal format by default.

- `sheltie-cargo footprint --plain <glob>` and the footprint workflow's `plain` input declare
  data files outside the build (e.g. `config/**`), so they no longer make footprints leaky.
- Files matching a `[[sequence]]` rule share a `seq:` target, so two changes adding
  migrations always go through the merge engine and are renumbered instead of both landing.
- `demo/cargo-shop`: the demo project and its pull request generator.
- `deploy/Dockerfile` takes a `BUILD_JOBS` build argument to cap compile jobs on shared hosts.
- `deploy/demo/`: deployment files and runbook for the public demo instance.

### Changed

- The Sheltie check run is redesigned: a clear headline per state, what happens next, the footprint's targets in a collapsible section, how a change landed and which tier resolved its conflict.
- Every comment Sheltie posts follows one format: what happened, why, what to do next.
- `shadow-report --json` is now `--format json`.

- Licensed under Apache-2.0.
- Logs are plain text when stderr is not a terminal.

- `sheltie-merge` now has subcommands: `sheltie-merge merge` (tiers 1-3) and `sheltie-merge replay`.

- MSRV raised from 1.85 to 1.94, required by sqlx 0.9.
- A closed change can be reopened (`Closed` → `Pending`); `Change.diff_hash` is optional until
  the footprint is computed.

### Fixed

- A merge-engine or replay result that was verified while `main` moved on landed the pull
  request's original head instead of the verified resolution; it now lands the resolution.
- The verified prefix (`sheltie/verified`) only moves forward; a CI re-run on a newer commit
  sent it back and forth.
- The merge and replay workflows replace an earlier result branch when a resolution is retried
  onto a newer head, instead of failing the push.
