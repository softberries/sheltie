# Sheltie — Parallel Merge Engine Design

Status: draft · Author: Krzysztof Grajek (Softberries) · Source of truth for implementation; propose changes via PR.

Sheltie replaces the serial merge queue with a scheduler that merges provably independent PRs in parallel, resolves conflicts in escalating tiers, and keeps `main` green through fast culprit-finding instead of serialization.

**Name.** Sheltie, after the Shetland Sheepdog, a herding breed that keeps a flock moving into one pen. Earlier drafts used the working name *Lattice*.

## Context and goals

Coding agents now open far more PRs than a one-at-a-time CI can land. Each merge invalidates every queued PR's verification, so throughput is capped near 1 / T_build and work is repeated.

**Goals**

- Land N PRs in roughly (N / k) × T_incremental, where k is the average width of independent sets.
- Never require a full rebuild for a merge that provably commutes with what landed before it.
- Resolve at least 90% of textual conflicts without a human, and say which tier resolved each one.
- Keep `main` green at every verified commit; bound any red window on the optimistic tip to one sweep interval.

**Non-goals**

- Replacing the forge. GitHub stays the source of truth for review, comments and branch protection.
- Replacing the build tool. Sheltie reads the existing build graph through adapters.
- Judging whether an agent's change is a good idea. Review policy stays where it is.

**Assumptions to confirm**

- Forge: GitHub, with a GitHub App holding merge rights on protected branches.
- Implementation language: Rust (control plane), with workers in any language.
- First adapter: Cargo workspaces; sbt/Mill and Nx/Turborepo next. The stack of the target repos is not yet confirmed.
- PRs from agents carry, or can carry, a structured intent: task spec, plan and acceptance tests.

## Core model

A PR is a transaction with a footprint; two PRs whose footprints don't intersect commute, so verifying each on the same base verifies their combination.

**Change unit.** One PR = `(base_sha, diff, intent?)`. The diff is treated as a cached artifact of `intent` applied to `base_sha`, and can be re-derived.

**Footprint.** For change A:

- W(A), the write set: changed files, symbols and build targets, plus their interface hashes before and after.
- R(A), the read set: targets whose inputs include W(A) *through a changed interface*, plus the tests covering those targets.

An implementation-only change to target M stops propagating at M; its dependents don't read it.

**Conflict relation.**

| Relation | Condition | Meaning |
| --- | --- | --- |
| Textual | W(A) ∩ W(B) ≠ ∅ at hunk level | Git can't apply both |
| Semantic | W(A) ∩ R(B) ≠ ∅ or W(B) ∩ R(A) ≠ ∅ | Both apply, but one may break the other |
| Independent | neither | Commute |

**Commutativity rule.** If A and B are each verified on base S and are independent, then S + A + B is verified with no further build. This holds only for targets whose builds are pure functions of declared inputs; everything else is covered by sweeps.

**Guarantee tiers on `main`.**

1. *Footprint-proven*: landed under the commutativity rule. Fast; about 95% of merges is the target.
2. *Speculation-verified*: landed after building the exact combined state.
3. *Sweep-verified*: covered by a full build of `main` at or after this commit. The verified prefix is the newest sweep-verified commit; everything after it is the optimistic tip.

## Architecture

Nine components; only the committer touches `main`, and it does a set intersection plus a compare-and-swap, so it is never the bottleneck.

```text
 Agent orchestrator ──► Intake (GitHub App) ── PR, base sha, intent
   │ /footprint, /leases        │
   └ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─► ▼
                       Footprint analyzer ◄── Build graph + cache (adapter CLIs on CI runners)
                                │
                                ▼
                            Scheduler ── conflict graph, epoch plan
                  textual       │        commuting sets
              ┌─────────────────┴─────────────────┐
              ▼                                   ▼
        Merge engine ── resolved change ──►   Verifier (impacted targets, cached)
        (tiers 1–5,                               │ green
         intent replay)                           ▼
              ▲ write conflict ─────────  Committer (serial: OCC check + CAS on head)
                                                  │
                                                  ▼
                                   main (verified prefix + optimistic tip) ──► Sweeper
                                                                 (full builds, bisect, auto-revert)
```

PRs flow top to bottom. Textual conflicts detour through the merge engine; commuting sets go straight to verification. A write conflict found at commit time sends the change back to the merge engine. The dashed path is the planner asking for footprints and leases before work starts.

## Footprint analysis

Footprints are computed per PR against its base, cached by `(base_tree_hash, diff_hash)`, and are the only input the scheduler uses to decide what may run in parallel.

**Build-graph snapshot** (`sheltie-graph`). Each adapter describes one tree as data, and one pure function computes the footprint from the base and head snapshots, so the algorithm is the same for every build tool and can be property-tested:

```rust
struct GraphSnapshot {
    targets: Map<TargetId, Target>,          // per target: see below
    owners: Map<RepoPath, Set<TargetId>>,    // files -> owning targets
}
struct Target {
    reads: Map<TargetId, EdgeKind>,          // Local (same crate) or Public (another crate)
    iface: IfaceHash,                        // seen by Local readers
    public_iface: IfaceHash,                 // seen by Public readers
    tests: Set<TestId>,                      // covering tests
    hermetic: bool,
    zone: Zone,                              // taint zone if not hermetic
}
fn footprint(base: &GraphSnapshot, head: &GraphSnapshot, changed: &Set<RepoPath>, extra_reads: &Set<TargetId>) -> Footprint;
```

**Algorithm**

1. Map changed paths to owning targets: W_targets.
2. For each target in W_targets, compare `interface_hash` before and after. Unchanged interface: stop. Changed: walk reverse dependencies one level and repeat.
3. R = the visited closure plus `covering_tests` of every visited target.
4. Any visited target that is not hermetic widens R to a configured *taint zone* (a directory or the whole repo) and marks the PR `leaky`.

**Adapters, in delivery order**

| Adapter | Graph source | Interface hash | Known leaks |
| --- | --- | --- | --- |
| Cargo | `cargo metadata` + module tree parsed with `syn` | Module signatures (bodies and docs removed), folded with the interfaces they name | `build.rs`, proc-macros, computed `include!` paths, integration tests using fs or network |
| sbt / Mill | task graph export | Zinc API hash per compilation unit | macros, resource generators |
| Nx / Turborepo | project graph JSON | `.d.ts` emit hash | runtime config, env vars |
| Fallback | directory ownership file | none (any change propagates) | everything; footprints are coarse |

**Adapter order:** Cargo first, then one BSP (Build Server Protocol) adapter covering sbt, Mill, Bazel and Gradle (via plugin), then a dedicated Maven adapter. sbt and Mill give the finest interface hashes through Zinc; Gradle exposes module ABIs through compile avoidance and the Tooling API; Maven has no ABI, so Sheltie hashes compiled class signatures itself (e.g. japicmp).

**Cargo adapter (`sheltie-cargo`).** Targets are modules (`pkg::billing::tax`), so changes to different modules of one crate commute; most Rust projects are a single crate. Sources are parsed with `syn` straight from git objects, with no build and no nightly toolchain (rustdoc JSON, and so `cargo-public-api`, is nightly-only). A module's interface is its item signatures with function bodies and docs removed (types, traits, constants with values, `macro_rules!` bodies, impl headers), folded with the interfaces of the modules those signatures name, so `pub use` and type aliases propagate; a module's public interface also folds its `pub` children. Edges come from resolving `use` and qualified paths; anything unresolvable falls back to coarser targets, never to fewer edges. Test modules (`#[cfg(test)]`) belong to their parent; integration tests read the crate's public interface. Cross-crate edges point at the crate's public interface. Files outside the module graph are ignored (docs), tracked without effect (`.github/**`, lint configs) or treated as leaky; `Cargo.lock` changes read the crates whose resolved dependencies changed. `sheltie-cargo graph` prints the snapshot for debugging.

**Where adapters run:** never in the control plane. Each adapter is a small CLI (`sheltie-cargo`, `sheltie-bsp`, `sheltie-maven`) run as a step on the customer's CI runner, where the toolchain already exists. It uploads a compact footprint file; the control plane only does set math and stays language-agnostic. Self-hosted worker pools get container images with each toolchain preinstalled.

**Upload.** The reusable workflow `.github/workflows/footprint.yml` runs the adapter on `pull_request` and posts the report to `POST /v1/footprints` with a GitHub Actions OIDC token (audience `sheltie` by default). Sheltie verifies the token against GitHub's published keys (RS256, issuer `https://token.actions.githubusercontent.com`) and accepts the upload only for the repository named in the token, and only if the App is installed there. Reports are stored by repository and head commit, so an upload can arrive before the webhook, and are validated (bounded sizes, writes ⊆ reads). The change's check run then shows the footprint.

*Open security question:* a token proves which repository's workflow ran, not that the workflow is trusted. Workflows of `pull_request` runs come from the PR's merge commit, so if GitHub granted OIDC tokens to runs from forked PRs, a fork could upload a footprint that is too small. GitHub's OIDC reference does not say how forks are treated; this must be verified before Sheltie gets merge rights (M4). Until then footprints only drive shadow-mode predictions, and wrong ones show up as footprint misses.

**Non-code hotspots** get explicit rules: lockfiles and generated files are *derived* (regenerate, never merge); migrations, OpenAPI specs and feature-flag configs are *global* (every writer conflicts with every reader).

**Measuring leakage.** Every sweep failure traced to a pair that Sheltie judged independent is logged as a footprint miss with the pair and the targets involved. That rate decides where hermeticity work pays off.

## Scheduler

The scheduler runs in epochs of a few seconds: it rebuilds the conflict graph of all ready PRs, then assigns each connected component one of three execution strategies.

**Ready** means: approved per branch protection, presubmit green on its own base, footprint computed.

**Per epoch**

1. Build graph G: nodes = ready PRs, edges = textual or semantic conflict.
2. Split G into connected components.
3. *Singleton component* (no edges): if its verification is on a base whose changes since then don't touch R(PR), send it straight to commit. Otherwise reverify only the intersected targets.
4. *Textual edges inside a component*: send to the merge engine first; resolved pairs re-enter as one combined change.
5. *Semantic-only components*: choose a strategy by size and risk.

| Strategy | When | How |
| --- | --- | --- |
| Speculative chain | component size ≤ 8 | Order by priority; build prefixes A, A+B, A+B+C concurrently; commit the longest green prefix |
| Weighted speculation | size ≤ 8, mixed risk | Also build branches that skip the riskiest PR; spend compute in proportion to predicted success |
| Group-tested batch | size > 8, low risk | Build the whole batch once; on failure, bisect; batch size about 1 / √p for failure rate p |

**Risk score** per PR, 0–1, learned from history: presubmit flakiness of touched tests, footprint size, `leaky` flag, the agent's own history, and whether intent replay was used.

**Fairness.** Age raises priority so a PR in a busy region of the graph can't starve. A PR that has lost three epochs gets a lease on its read set: newer writers into that set wait one epoch.

**Capacity control.** Workers are a shared pool; independent singletons are cheap (often cache hits) and are always scheduled first, speculation gets what remains.

## Merge engine

Conflicts are resolved by the cheapest tier that succeeds, per file; every resolution is recorded with its tier and must pass verification like any other change.

| Tier | Mechanism | Applies to | Cost |
| --- | --- | --- | --- |
| 0 | No conflict: commute | Independent footprints | None |
| 1 | Structural merge on the AST (e.g. mergiraf) | Both sides add imports, enum variants, match arms, routes | Milliseconds |
| 2 | Regenerate derived files | Lockfiles, generated code, snapshots, `CHANGELOG` | One generator run |
| 3 | Declared merge semantics per path | Append-only registries, migration lists, config sets | Milliseconds |
| 4a | Intent replay on the conflicting hunks | Remaining textual conflicts | One scoped agent run |
| 4b | Intent replay of the whole change on the new base | 4a failed, or semantic conflict broke verification | One full agent run |
| 5 | Human | 4b failed twice, or the intents contradict each other | Review round-trip |

**Merge rules file** (`.sheltie/merge.toml`, checked in) declares tier 2 and 3 behaviour:

```toml
[[derived]]
paths = ["Cargo.lock"]
regenerate = "cargo update -w --offline"

[[derived]]
paths = ["src/generated/**"]
regenerate = "make codegen"

[[set]]          # order-insensitive lines, union both sides
paths = ["config/feature_flags.txt"]

[[sequence]]     # renumber the later side on collision
paths = ["migrations/*.sql"]
key = "^(\\d+)_"
```

**Intent replay contract.** A PR is replayable when it carries `intent.json`: task text, the plan the agent followed, and acceptance tests that must pass. Replay gives the agent the new base, the original diff as a hint, and the conflicting hunks. The result is accepted only if:

- the PR's own acceptance tests pass;
- the diff stays inside the original W(A), widened by at most the conflicting files;
- a cheap similarity check against the original diff stays above a threshold, otherwise the PR goes back to review.

Replayed PRs are labelled on GitHub and keep the original approval only when the similarity check passes; otherwise they need re-approval.

## Verification and commit protocol

`main` advances only through one serialized compare-and-swap, which is cheap; everything expensive happens before it, in parallel.

**Verifier.** Runs only the targets and tests in R(change), against a content-addressed remote cache (sccache, Gradle/Mill cache, or Bazel remote-exec if adopted). A verification record is `(tree_hash, target_set, result)` and is reusable by any later change whose relevant inputs hash the same.

**Commit (optimistic concurrency control)**

1. A change C arrives verified on base S.
2. Read the current head H. Let D = the commits between S and H.
3. If W(D) ∩ R(C) = ∅ and W(D) ∩ W(C) = ∅: apply C onto H, CAS head from H to H + C. Done.
4. If only R(C) intersects: reverify just the intersected targets on H + C, then retry from step 2.
5. If W intersects: return C to the merge engine.
6. A CAS loss (head moved) retries from step 2; it costs a set intersection, not a build.

The resulting history is linear on GitHub. Each commit gets a trailer recording its guarantee tier and base, e.g. `Sheltie-Verified: footprint S=ab12cd`.

**Sweeps.** A full build and test of the head runs on a fixed cadence (start: every 20 minutes or every 50 commits, whichever comes first). A green sweep advances the verified prefix.

**Culprit finding.** On a red sweep:

1. Bisect the commits since the last green sweep, using cached results where possible.
2. Auto-revert the culprit with a PR that lands through Sheltie at top priority.
3. If the culprit has an intent, open a re-derivation PR on the new head; otherwise notify the author.
4. If the culprit was footprint-proven, log a footprint miss naming the pair of changes involved.

**Deploy gate.** Release pipelines read from the verified prefix, never from the optimistic tip.

## Data model and APIs

State lives in Postgres (relational records and the commit CAS) plus the existing build cache for verification artifacts; the control plane is stateless and can run as several replicas.

**Core records**

```rust
struct Change {
    id: ChangeId,              // GitHub repo id + PR number + head sha
    base: CommitSha,
    diff_hash: DiffHash,
    intent: Option<IntentRef>, // intent.json blob
    state: ChangeState,        // Pending | Ready | Merging | Verifying | Committable | Landed | Returned | Closed
    risk: Score,               // 0.0..=1.0
    priority: i64,
}

struct Footprint {
    change: ChangeId,
    base_tree: TreeHash,
    writes: TargetSet,
    reads: TargetSet,
    tests: TestSet,
    leaky: bool,
}

struct Verification {
    tree: TreeHash,
    targets: TargetSet,
    result: Outcome,           // Pass | Fail(test_ids) | Flaky
    runner: RunnerId,
}

struct Resolution {
    changes: Vec<ChangeId>,
    tier: ResolutionTier,      // Commute | Structural | Regenerate | Declared | ReplayHunks (4a) | ReplayFull (4b) | Human
    files: Vec<RepoPath>,
    similarity: Option<Score>, // replay tiers only
}

struct HeadEvent {
    seq: u64,                  // CAS sequence
    commit: CommitSha,
    change: ChangeId,
    tier: GuaranteeTier,
}
```

`ChangeState` adds `Pending` (open, not yet ready) and `Closed` (closed or superseded) to the pipeline states; allowed transitions are defined in `sheltie-core`. `Landed` is terminal; `Closed` returns to `Pending` only when the PR is reopened at the same head. A push to a PR creates a new `ChangeId` and closes the previous head's change. The resolution tier is an enum rather than `u8` because tier 4 has two variants (4a, 4b).

**Intake.** Every webhook is verified (HMAC-SHA256) before parsing, then stored in one transaction with its delivery id, which drops GitHub's redeliveries. GitHub does not guarantee delivery order, so lifecycle events are ordered by the PR's `updated_at`; an event older than one already applied is logged but changes nothing. Check runs are not posted from the webhook handler: intake marks the change's check run as out of date (an outbox row), and a worker claims rows with a short lease and posts them, retrying with backoff. Intake therefore keeps working while GitHub's API is down, and several replicas can run the worker.

**Interfaces**

| Surface | Direction | Purpose |
| --- | --- | --- |
| GitHub App webhooks | in | PR opened, updated, approved, closed |
| GitHub check runs | out | Per-PR status: footprint, tier, queue position, resolution |
| `POST /changes/{id}/intent` | in | Agents attach or update an intent |
| `GET /footprint?paths=…` | out | Task planners ask for a predicted footprint before assigning work |
| `POST /leases` | in | Planner reserves a read/write region for a task (soft lock, TTL) |
| Worker protocol (gRPC) | both | Lease a verification or replay job, stream logs, report result |
| Event stream (NATS or Kafka) | out | Every state change, for dashboards and conflict telemetry |

The `/footprint` and `/leases` endpoints let the agent orchestrator prevent conflicts before code is written, not only resolve them afterwards.

## Failure modes and risks

The main risk is footprints that are wrong in the optimistic direction; every mitigation below either measures that or shrinks the damage window.

| Failure | Effect | Mitigation |
| --- | --- | --- |
| Non-hermetic target judged independent | Red sweep after landing | `leaky` flag widens R; footprint-miss log drives hermeticity fixes |
| Coupling outside the build graph (DB schema, runtime config, reflection) | Semantic conflict not seen | Declare such paths `global` in merge rules |
| Flaky tests | False culprits, wasted bisects | Quarantine list; a test must fail twice on the same tree to count |
| Intent replay changes behaviour silently | Wrong code with green tests | Diff confined to W(A); similarity check; re-approval below threshold |
| Hot region (shared registry, central module) | Large components, speculation explodes | Leases at planning time; hotspot report for refactoring |
| Sheltie outage | Nothing lands | Fall back to GitHub's native merge queue; Sheltie holds no state GitHub needs |
| Revert storm (culprit reverted, dependents break) | Cascading reverts | Revert includes dependents landed after it, then re-derives them via intent |
| Security: agent-supplied merge rules or intents | Rules that hide changes | Merge rules and intent schema changes require CODEOWNERS review |

## Rollout

Ship in four phases, each usable on its own and each gated by a measured result, so the project pays off before intent replay exists.

| Phase | Scope | Gate to the next phase |
| --- | --- | --- |
| 1 Parallel queue | Cargo adapter, conflict graph, OCC commit, sweeps and bisect; shadow mode first | Footprint misses under 1 per 1,000 merges |
| 2 Structural merge | Tiers 1–3, `merge.toml` rules, speculative chains | 70%+ of conflicts auto-resolved |
| 3 Intent replay | Tier-4 replay, auto-repair PRs, similarity check | 80%+ of replays accepted |
| 4 Prevention | `/footprint`, `/leases`, hotspot report, more adapters | — |

Phase 1 runs in shadow mode first: Sheltie computes footprints and predicts merge order beside the current queue, and the footprint-miss rate is measured before Sheltie gets merge rights. Each later phase can be skipped or reordered if its gate isn't met.

**Metrics** (all emitted on the event stream)

| Metric | Why it matters |
| --- | --- |
| PRs landed per hour; p50 / p95 time from ready to landed | The throughput the project exists for |
| Average independent-set width k | The ceiling on parallelism; rises with modularity |
| Share of merges by guarantee tier | How much rests on footprint proofs |
| Footprint-miss rate per 1,000 merges | Whether footprint proofs are trustworthy |
| Conflicts resolved per tier; human escalations per day | Whether the merge engine is pulling its weight |
| Red minutes on the optimistic tip per day | The cost of optimism |
| CI compute per landed PR | Whether speculation is paying for itself |

## Open questions

- [ ] Which repos go first, and what build tool and language do they use? This decides the first adapter.
- [ ] Is the agent orchestrator able to emit `intent.json` with acceptance tests, and call `/leases` before assigning a task?
- [ ] What sweep cadence is acceptable given full-build cost?
- [ ] Does replayed code keep the original approval above the similarity threshold, or always need re-approval?
- [ ] Build or extend: is an existing merge queue (GitHub merge queue, Mergify, Graphite, Trunk) a viable host for the scheduler, with Sheltie supplying footprints only?

- [ ] Product direction for Softberries: a hosted SaaS (open core), or mainly a showcase of Softberries' expertise? Decide after open-sourcing the footprint engine, based on who shows interest.

## Licensing and monetization notes

The plan is open source first, with no monetization on day one. Customers self-host the control plane and bring their own GitHub App, CI runners (for example GitHub Actions) and agent API keys, so the project bears no running costs.

**Later monetization paths**

- Hosted Sheltie: install as a GitHub App, no ops.
- Enterprise features outside the open version: SSO, audit logs, org-wide queues, analytics, policy controls.
- Managed agent runs: included agent credits instead of the customer's own keys.
- Support and consulting through Softberries.

**AGPL drawbacks**

- Many companies ban AGPL outright because it applies to network use, which blocks adoption at large agent-heavy teams.
- It deters contributors and integrators who want to avoid copyleft.
- A closed enterprise edition requires a CLA from every contributor, and some won't sign.
- It doesn't fully stop large clouds from hosting it, as long as they publish their changes.

**Alternatives:** Apache 2.0 with a closed enterprise layer (open core), or the Business Source License, which converts to Apache after a few years. Suggestion, not yet decided: Apache plus a closed enterprise layer, because it hurts adoption least for self-hosted infrastructure.

## Implementation plan (for Claude Code)

Build Sheltie as a Rust workspace in 9 milestones (M0–M8); each milestone is a set of tasks small enough for one Claude Code session, ends with passing tests, and leaves a usable system. M0–M4 match rollout phase 1 (shadow mode first).

**Repository layout**

```text
sheltie/
  crates/
    sheltie-core/        # domain types: Change, Footprint, Verification, Resolution, HeadEvent
    sheltie-graph/       # BuildGraph trait + footprint algorithm
    sheltie-cargo/       # Cargo adapter (cargo metadata, cargo-public-api)
    sheltie-github/      # GitHub App: webhooks, checks, labels, branches, merges
    sheltie-scheduler/   # conflict graph, epochs, strategies
    sheltie-merge/       # tiers 1-3, merge.toml, tier-4 replay client
    sheltie-commit/      # OCC check + CAS on head
    sheltie-sweeper/     # sweeps, bisect, auto-revert
    sheltie-store/       # Postgres (sqlx) + migrations
    sheltie-api/         # REST (axum) + MCP server
    sheltie-server/      # binary wiring it all together
  fixtures/              # sample repos + scripted conflict scenarios
  workflows/             # reusable GitHub Actions: verify, replay
  deploy/                # Dockerfile, docker-compose, Helm chart
  docs/
  CLAUDE.md              # conventions for Claude Code
```

**Stack:** Rust (tokio, axum, sqlx, octocrab, petgraph, serde, tracing), Postgres, GitHub Actions as the default runner backend, mergiraf for structural merge, Claude Code headless as the default replay agent.

**Milestones**

1. **M0 — Skeleton.** Workspace, CI, `CLAUDE.md`, core types, Postgres schema and migrations, docker-compose with Postgres. Done when: `cargo test` passes and the server starts.
2. **M1 — GitHub App intake.** App manifest, webhook receiver with signature check, store PR open/update/approve/close, post a "Sheltie" check run per PR. Done when: a test repo's PRs show a Sheltie check.
3. **M2 — Footprints (Cargo).** `BuildGraph` trait, Cargo adapter, interface hashing via `cargo-public-api`, footprint algorithm with caching and a `leaky` flag. Starts with module-level targets inside a crate, since most Rust projects are a single crate and crate-level targets would make every PR conflict (see `docs/backlog.md`, M2). Done when: golden tests on a sample workspace give expected write/read sets.
4. **M3 — Shadow mode.** Conflict graph per epoch; predict merge batches and write them to the check run without merging. Log predictions vs. what actually happened. Done when: a week of shadow data on a real repo with a footprint-miss report.
5. **M4 — Parallel commit.** Verification via GitHub Actions on temporary branches; OCC commit with CAS; commit trailers; sweeper with bisect and auto-revert PRs. Done when: independent PRs land in parallel on a test repo and a seeded bad commit is reverted automatically.
6. **M5 — Merge tiers 1–3 and speculation.** mergiraf integration, `merge.toml` (derived, set, sequence), speculative chains for small components, group-tested batches. Done when: lockfile and import conflicts resolve with no human.
7. **M6 — Intent replay.** `intent.json` schema, replay workflow running Claude Code headless with the user's key, acceptance-test check, diff-scope and similarity checks, re-approval labels, budgets and per-PR caps. Done when: a seeded semantic conflict is re-derived and lands.
8. **M7 — Coordination.** Issue footprint prediction, leases, MCP server (`safe_issues`, `claim_issue`, `predict_footprint`, `release`), GitHub labels and comments for non-MCP agents. Done when: two agents given overlapping issues are serialized automatically.
9. **M8 — Release.** Helm chart, docs site, opt-in anonymous telemetry, example repo, license and CONTRIBUTING. Done when: a stranger can self-host from the README in under 30 minutes.

**How to drive it with Claude Code**

- This doc lives in `docs/design.md`; `CLAUDE.md` points to it and lists conventions (error handling, tests required, no unsafe).
- One milestone per branch; ask Claude Code to plan the milestone first, then implement task by task with tests.
- Keep a sample Cargo workspace in `fixtures/` with scripted conflict scenarios; every milestone's "done when" is an integration test against it.
- After M4, dogfood: Sheltie merges its own PRs.

**Public demo repositories (end-to-end showcase).** For each supported technology (Cargo first, then sbt, Gradle, Maven), create a public demo repository with a real project, plus a script that generates a batch of agent-style PRs, many of them deliberately conflicting (textual, lockfile, registry and semantic conflicts). Sheltie, integrated as a GitHub App, merges them end to end in the open: parallel landing, tier-by-tier conflict resolution, intent replay and auto-revert. Each run publishes its results (PRs landed, time, tiers used, human escalations) as a public benchmark and demo.

## Related documents

- `docs/backlog.md` — issue backlog by milestone, including the M4.5 early public Cargo demo
- `docs/playground.md` — test playground and GitHub App setup
- `docs/roadmap.md` — post-v1 roadmap
- `CLAUDE.md` — conventions for Claude Code
