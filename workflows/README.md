# Reusable GitHub Actions workflows

Workflows that customer repositories call so Sheltie can use their runners and toolchains.
GitHub only allows reusable workflows under `.github/workflows/`, so they live there:

- [`footprint.yml`](../.github/workflows/footprint.yml) (M2): builds `sheltie-cargo`, computes the pull
  request's footprint and uploads it, authenticated by a GitHub Actions OIDC token. The caller
  grants `id-token: write`; see the comment at the top of the file.
- [`merge.yml`](../.github/workflows/merge.yml) (M5): the merge engine. Triggered by Sheltie's
  `repository_dispatch` (`sheltie-merge`); merges the pull request onto the head with mergiraf,
  regenerated derived files and declared rules, pushes the result to `sheltie/merge/*` and
  reports to Sheltie. See the comment at the top of the file.
- Verification (M4) needs no workflow: Sheltie pushes `sheltie/verify/*` and `sheltie/spec/*`
  branches and the repository's own CI runs on them.
- `replay.yml` (M6): run the intent-replay agent (Claude Code headless) with the customer's own key.

The control plane never runs build tools.
