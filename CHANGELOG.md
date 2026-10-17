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

### Changed

- MSRV raised from 1.85 to 1.94, required by sqlx 0.9.
- A closed change can be reopened (`Closed` → `Pending`); `Change.diff_hash` is optional until
  the footprint is computed.
