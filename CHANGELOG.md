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

### Changed

- MSRV raised from 1.85 to 1.94, required by sqlx 0.9.
- A closed change can be reopened (`Closed` → `Pending`); `Change.diff_hash` is optional until
  the footprint is computed.
