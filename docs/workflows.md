# Workflows

Sheltie never runs your build in the control plane. Three reusable GitHub Actions workflows run
on your runners and report to Sheltie, authenticated with GitHub's OIDC tokens (no shared
secret). Add them to the default branch of each repository Sheltie manages, replacing
`https://sheltie.example.com` with your server's URL.

Pin the workflows to a release, as below: `@v0.5.0` selects the workflow, and `sheltie-version`
downloads that release's checksum-verified binaries instead of building them from source, which
takes a few minutes per run. `@main` with no `sheltie-version` follows the latest code.

## Footprints (required)

`.github/workflows/sheltie-footprint.yml`:

```yaml
on:
  pull_request:
  push:
    branches: [main]     # your default branch
permissions:
  contents: read
  id-token: write        # lets the job request the OIDC token
jobs:
  footprint:
    uses: softberries/sheltie/.github/workflows/footprint.yml@v0.5.0
    with:
      sheltie-version: v0.5.0
      sheltie-url: https://sheltie.example.com
```

On pull requests it computes the footprint with the adapter for your [build
tool](build-tools.md) and uploads it. On pushes to the default branch it uploads the build
graph, which predicts footprints of issues (see [Coordinating agents](agents.md)).

The adapter is picked from the files at the repository root: `Cargo.toml` (Cargo), `go.mod`
or `go.work` (Go), `pom.xml` (Maven), `settings.gradle(.kts)` or `build.gradle(.kts)`
(Gradle), `package.json` (JavaScript/TypeScript), `pyproject.toml`, `setup.py` or
`setup.cfg` (Python), in that order. For a repository with several, set it:

```yaml
    with:
      sheltie-url: https://sheltie.example.com
      tool: node   # cargo, go, node, python, maven or gradle
```

Files outside every unit (crate, package, module) are assumed to affect the whole build, so a change to one conflicts
with everything. Declare data files that do not, one glob per line:

```yaml
    with:
      sheltie-url: https://sheltie.example.com
      plain: |
        config/**
        migrations/**
```

Two changes to the same plain file still conflict (and go to the merge engine).

### Pull requests from forks

GitHub gives workflow runs for pull requests from forks a read-only token and no OIDC token,
so their footprint cannot be uploaded. The workflow then skips the upload with a notice
instead of failing, and the Sheltie check keeps waiting for the footprint: Sheltie never
lands a fork's pull request. Review and merge it by hand as usual. Once it is on the default
branch, sweeps cover it like any other commit, and because its footprint is unknown, ready
pull requests are rebuilt on top of it before they land. Fork pull requests are not
[replayed](intent-replay.md) either.

## Merge engine (optional)

Resolves conflicts with tiers 1–3 (see [Merge rules](merge-rules.md)). Sheltie starts it with
a `repository_dispatch` event. `.github/workflows/sheltie-merge.yml`:

```yaml
on:
  repository_dispatch:
    types: [sheltie-merge]
permissions:
  contents: write        # pushes the merge result to sheltie/merge/*
  id-token: write
  pull-requests: read
jobs:
  merge:
    uses: softberries/sheltie/.github/workflows/merge.yml@v0.5.0
    with:
      sheltie-version: v0.5.0
      sheltie-url: https://sheltie.example.com
      pr: ${{ github.event.client_payload.pr }}
      head: ${{ github.event.client_payload.head }}
      onto: ${{ github.event.client_payload.onto }}
```

Then turn it on: `sheltie-server repo set-merge-engine --repo owner/name --state on`.

## Intent replay (optional)

Re-derives a change with Claude Code when it cannot be merged otherwise. Needs your
`ANTHROPIC_API_KEY` as a repository secret; see [Intent replay](intent-replay.md) for the
workflow and its settings.

## Your CI

Sheltie relies on your existing CI:

- A pull request is ready to land when it is approved (or needs no review) and all its checks
  are green.
- In merge mode, CI must run on pushes to branches under `sheltie/`: changes are verified on
  `sheltie/verify/*`, speculative builds run on `sheltie/spec/*`, and bisection uses
  `sheltie/bisect/*`.
- Sweeps read CI results of the default branch's commits, and `sheltie/verified` points at the
  newest green one. Deploy from it if you want only verified commits.
