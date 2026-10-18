# Sheltie

Sheltie is a parallel merge engine for agent-generated pull requests. It runs as a GitHub App
next to your existing CI.

Coding agents open more pull requests than a one-at-a-time merge queue can land. Sheltie
computes a *footprint* for every pull request: the build targets it writes and the targets it
affects. Pull requests with independent footprints land in parallel. Conflicting ones are
built together speculatively or resolved in escalating tiers:

1. structural merge (mergiraf);
2. regenerating derived files such as lockfiles;
3. merge rules you declare;
4. re-deriving the change from the agent's recorded intent with Claude Code.

Before work starts, Sheltie can also keep agents off overlapping issues.

## How a pull request lands

1. An agent opens a pull request; GitHub notifies Sheltie.
2. An adapter for your build tool ([Cargo, Go, JavaScript/TypeScript, Python, Maven or
   Gradle](build-tools.md)) runs on your CI runner and uploads the footprint.
3. Once the pull request is approved and its checks are green, the scheduler decides each
   epoch: land now, verify on the current head, build speculatively with others, or merge
   with the merge engine.
4. The committer checks that nothing that landed meanwhile touches the change, then
   fast-forwards the default branch. It never forces.
5. Sweeps watch the default branch's CI; a red commit is bisected and reverted automatically.

Builds, tests and agent jobs run on **your** runners with **your** API keys. The control
plane only does set arithmetic and talks to GitHub.

## Status

Sheltie is early software (0.x). Start in *shadow mode*, where Sheltie only predicts what it
would do and reports how often it would have been wrong, before letting it merge.

Supported build tools: Cargo. sbt, Mill, Gradle, Bazel and Maven are planned.

## Next

- [Self-hosting](self-hosting.md): run your own Sheltie.
- [Workflows](workflows.md): what to add to your repositories.
