# Reusable GitHub Actions workflows

Workflows that customer repositories call so Sheltie can use their runners and toolchains.
GitHub only allows reusable workflows under `.github/workflows/`, so they live there:

- [`footprint.yml`](../.github/workflows/footprint.yml) (M2): builds `sheltie-cargo`, computes the pull
  request's footprint and uploads it, authenticated by a GitHub Actions OIDC token. The caller
  grants `id-token: write`; see the comment at the top of the file.
- `verify.yml` (M4): build and test the impacted targets of a temporary Sheltie branch, report a check.
- `replay.yml` (M6): run the intent-replay agent (Claude Code headless) with the customer's own key.

The control plane never runs build tools.
