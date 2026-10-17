# Sheltie demo: a Cargo shop

A small Rust workspace (pricing, catalog, cart, invoice, search, auth) that
[Sheltie](https://sheltie.dev) merges. `scripts/demo-prs.sh` opens a batch of agent-style
pull requests, and Sheltie lands them. The footprints and conflicts below were checked with
`sheltie-cargo` (the demo marks `config/**` and `migrations/**` as plain data files):

| Scenario | Pull requests | What Sheltie does |
| --- | --- | --- |
| Independent changes | one per crate: search, auth, catalog | writes one crate each; lands them together |
| Feature flags | two PRs appending flags | same file: the merge engine unions it (tier 3) |
| Migrations | two PRs adding `0002_…` | shared `seq:` target: the merge engine renumbers the later one (tier 3) |
| Semantic conflict | a rename, and a new caller of the old name | merges cleanly but fails CI; re-derived from its intent (tier 4b) |

Watch it on the pull requests (the **Sheltie** check) and on the
[dashboard](https://demo.sheltie.dev/dashboard).
