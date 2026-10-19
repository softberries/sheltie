# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- ROI report: each change is replayed against the branch it was tested on (the merge base of
  its pull request head, from merge commits or fetched `refs/pull/*/head`), so Sheltie's
  rebuilds after overlapping landings are counted. The report says how many changes had no
  known base; those still count as tested when they landed, which favours Sheltie.
- `sheltie-go`: files a package embeds with `//go:embed` are resolved from the tree (the go
  command's pattern rules) and belong to the package, which stays hermetic. Only a pattern
  that cannot be resolved makes it leaky.

### Fixed

- `sheltie-cargo`: modules declared with a raw identifier (`mod r#type;`) or with a
  `#[path]` in a file that is not `mod.rs` are found, instead of making footprints leaky.
- Adapters: two checkouts of the same commit in one process no longer collide on the
  worktree directory name.
- `sheltie-cargo`: modules declared inside a macro invocation (`cfg_rt! { mod pool; }`, as
  tokio does) are followed, instead of their files counting as outside the module graph.
- `sheltie-node`: a `package.json` starting with a UTF-8 byte order mark is read, as Node
  reads it.
- `.circleci/**` is a plain file for every adapter, like `.github/**` and `.gitlab-ci.yml`.

## [0.4.0] - 2026-10-19

### Added

- Adapters for more build tools, each a binary shipped with every release and picked by the
  footprint workflow (new `tool` input, `auto` by default):
  - `sheltie-go`: Go modules; packages, imports, exported declarations.
  - `sheltie-node`: JavaScript and TypeScript in npm, yarn and pnpm workspaces and Nx;
    workspace packages, dependencies and imports, exports and types.
  - `sheltie-python`: Python (uv workspaces, setuptools, plain packages); import packages,
    imports, public signatures and re-exports.
  - `sheltie-maven` and `sheltie-gradle`: Maven reactors and Gradle builds; modules and
    projects, their dependencies, the public Java and Kotlin API.
  They read build files and sources from git, without the build tool or a compiler. See
  `docs/build-tools.md`.
- `sheltie-adapter`, the shared core every adapter (including `sheltie-cargo`) builds on.

### Changed

- A dependency a change adds now counts as read by it, so it meets a concurrent change to
  that dependency's interface.
- Files such as `CHANGES.rst`, `*.adoc`, `AUTHORS` and `NOTICE` are ignored by default, like
  Markdown.

### Fixed

- The footprint workflow no longer fails on pull requests from forks, which get no OIDC
  token: it skips the upload with a notice, and Sheltie leaves the pull request to be merged
  by hand. See "Pull requests from forks" in `docs/workflows.md`.

### Upgrading

- Bump `@v0.3.0` and `sheltie-version` in your workflows to `v0.4.0`. Repositories that are
  not Cargo need no other change: the footprint workflow detects the build tool, or set
  `tool` explicitly.

## [0.3.0] - 2026-10-18

### Added

- Dashboard: a "Parallelism" panel per repository estimating how many agents can work at
  once while at least 80% of their changes land without a combined build or the merge engine,
  from the footprints of the last 90 days of merged pull requests. It shows the clean share
  for 1 to 64 agents and the targets behind most conflicts. See `docs/dashboard.md`.
- `sheltie-cargo roi`: replays the last 90 days of a branch through the scheduler, locally,
  and reports the waiting time and CI runs saved against a one-at-a-time merge queue, what
  Sheltie would have done with each change, and the parallelism estimate. Footprints are
  predicted from changed files, or computed per landing with `--exact`. See `docs/roi.md`.
- Agent scorecards: a dashboard panel and `sheltie-server scorecard` with, per agent, pull
  requests opened, merge rate, returns, reverts, replays, review rounds, time to merge and
  replay cost per merged pull request. Agents are named by `[[agents]]` rules in
  `.sheltie/merge.toml` (author, branch prefix or label); bots count as themselves and people
  are grouped. Sheltie now records each pull request's author, branch and labels from its
  webhooks. See `docs/dashboard.md#agents`.
- Replay reports carry the model the agent used most (`model`), shown on the agent spend
  panel.

### Upgrading

- One database migration (pull request author, branch and labels; replay model; agent
  rules); the server applies it on startup. Pull requests opened before the upgrade show as
  "Not recorded" in the scorecards until their next event.
- Bump `@v0.2.0` and `sheltie-version` in your workflows to `v0.3.0` so replay reports
  include the model.

## [0.2.0] - 2026-10-18

### Added

- Dashboard: an "Agent spend" panel per repository, showing the estimated cost of intent
  replays over the last 30 days, per day and against `daily_budget_usd` for the last 24 hours,
  with token counts, cost per replay and spend per hour of author time saved. The overview
  shows the spend per repository and in total. See `docs/dashboard.md`.
- Replay reports carry the agent's token counts (`tokens`: input, output, cache read, cache
  write) from Claude Code's `modelUsage`, or `usage` on older versions. The field is optional,
  so older runners still report.

### Upgrading

- One database migration (token counts on replay runs and the daily budget per repository);
  the server applies it on startup. Bump `@v0.1.0` and `sheltie-version` in your workflows to
  `v0.2.0` so replay reports include token counts.

## [0.1.0] - 2026-10-17

The first public release.

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
- Dashboard: a "Time saved" panel per repository and totals on the overview. Waiting time is
  measured against a one-at-a-time merge queue simulated from real ready times and the median
  measured CI time on the default branch; author time counts resolved conflicts at assumed
  minutes, set with `[estimates]` in `.sheltie/merge.toml`. See `docs/dashboard.md`.
- Dashboard redesign: Geist typography (self-hosted, SIL OFL), a Shetland palette, landings-per-day
  charts, a landing timeline, CSS-only motion that respects reduced-motion settings.
- CI durations of default-branch commits are recorded (`branch_commits.ci_seconds`).
- sheltie.dev and the docs use the dashboard's Shetland palette and Geist (self-hosted), with an
  animated aurora hero and call-to-action band, gradient accents and hover glows.
- A new logo: an illustrated sheltie in profile under the Shetland aurora, as PNGs (favicon, touch
  icon, 128/256/512 px), and a matching social card (source in `scripts/social-card/`).

### Changed

- The dashboard's empty state says to sign in again after installing the App, since the
  repositories a user can see are read at sign-in.
- The sweeper logs a red default branch with no known green commit once per tip, not every tick.
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

- A lone red commit on the default branch is no longer left red: Sheltie runs its CI again on
  `sheltie/bisect/<sha>` and reverts it if it fails twice, or treats it as flaky if it passes.
- Branch CI ignores Sheltie's own check run, which reflects its verdict on a pull request, not CI
  (a returned pull request's head was reported as the default branch turning red).
- A merge or replay workflow that fails before reporting now tells Sheltie (`"failed": true` for
  merges), so the change is returned or escalated at once instead of after a timeout.
- GitHub API requests time out (10 s to connect, 30 s per request) instead of stalling a worker.
- Adding a valid `sheltie-intent` block to a returned pull request's description now retries it,
  as the returned comment says; before, only a new push did.
- A merge-engine or replay result that was verified while `main` moved on landed the pull
  request's original head instead of the verified resolution; it now lands the resolution.
- The verified prefix (`sheltie/verified`) only moves forward; a CI re-run on a newer commit
  sent it back and forth.
- The merge and replay workflows replace an earlier result branch when a resolution is retried
  onto a newer head, instead of failing the push.

[Unreleased]: https://github.com/softberries/sheltie/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/softberries/sheltie/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/softberries/sheltie/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/softberries/sheltie/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/softberries/sheltie/releases/tag/v0.1.0
