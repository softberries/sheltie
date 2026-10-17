# Demo kit

`cargo-shop/` is the content of the public demo repository: a small Cargo workspace with CI,
the Sheltie workflows, `.sheltie/merge.toml`, and `scripts/demo-prs.sh`, which opens a batch
of agent-style pull requests covering each way Sheltie lands changes (see its README).

To run the demo: create a repository from `cargo-shop/`, install the Sheltie App on it, set the
repository variable `SHELTIE_URL` (and the `ANTHROPIC_API_KEY` secret for intent replay), switch
it to merge mode with the merge engine on, then run `scripts/demo-prs.sh` in a clone.
