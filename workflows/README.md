# Reusable GitHub Actions workflows

Workflows that customer repositories call so Sheltie can use their runners and toolchains:

- `verify.yml` (M4) — build and test the impacted targets of a temporary Sheltie branch, report a check.
- `footprint.yml` (M2) — run an adapter CLI such as `sheltie-cargo` and upload the footprint.
- `replay.yml` (M6) — run the intent-replay agent (Claude Code headless) with the customer's own key.

The control plane never runs build tools itself.
