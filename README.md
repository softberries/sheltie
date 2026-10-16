# Sheltie

**A parallel merge engine for agent-generated pull requests.**

Coding agents open more PRs than a one-at-a-time merge queue can land. Sheltie runs as a GitHub App
next to your existing CI. It computes a *footprint* for every PR (which build targets it writes and
which it affects), lands independent PRs in parallel, and resolves conflicts in escalating tiers:
structural merge, regenerating derived files, declared merge rules, and finally re-deriving the change
from the agent's recorded intent.

> **Status: pre-alpha.** Nothing works yet; this repository holds the design and the project skeleton.
> Follow progress in [`docs/backlog.md`](docs/backlog.md).

## How it works

1. An agent opens a PR; GitHub notifies Sheltie.
2. An adapter CLI (`sheltie-cargo` first) runs on your CI runner and uploads the PR's footprint.
3. The scheduler builds a conflict graph: PRs with disjoint footprints are verified in parallel,
   conflicting ones go to the merge engine or a speculative build.
4. The committer checks that nothing that landed meanwhile touches the PR, then fast-forwards `main`.
5. Periodic sweeps build `main` in full; a failure is bisected and reverted automatically.

Builds, tests and agent repair jobs run on **your** runners with **your** API keys. The control plane
only does set arithmetic and talks to GitHub.

Full design: [`docs/design.md`](docs/design.md).

## Supported build tools

| Build tool | Status |
| --- | --- |
| Cargo | Planned for v1 (M2) |
| sbt, Mill, Gradle, Bazel (via BSP) | Post-v1 |
| Maven | Post-v1 |

## Development

Requirements: Rust (installed via rustup; the toolchain is pinned in `rust-toolchain.toml`), Docker.

```sh
cp .env.example .env
docker compose -f deploy/docker-compose.yml up -d   # Postgres
cargo build --workspace
cargo test --workspace                              # DB tests read DATABASE_URL from .env
cargo run -p sheltie-server                         # then: curl localhost:8080/healthz
```

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the full workflow and [`CLAUDE.md`](CLAUDE.md) for the
conventions used when working with Claude Code.

## License

To be decided before the first release (see "Licensing and monetization notes" in the design doc).
Until then, no license is granted.

---

Built by [Softberries](https://softberries.dev).
