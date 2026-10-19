# Sheltie on real repositories: method and data

Everything behind [Sheltie on real repositories](../../docs/real-world.md): the scripts that
collected the data, the bench that computed footprints and replayed queues, and the results.
The study window is the 90 days to 2026-10-19 (landings from 2026-07-21).

Raw inputs (clones, GitHub API responses, about 24,000 footprints) are not committed; the
scripts regenerate them. `results/` holds every number the write-up uses, and
`results/tables.md` is generated from it.

## What you need

- `git`, `gh` (logged in; the public API is enough), Python 3 (run with 3.14), a Rust toolchain.
- The build tools the adapters call, for exact footprints: `cargo` for Rust repositories.
  The other adapters read the tree from git.
- Disk: the clones took 5.8 GB.

## Steps

```sh
cd research/real-world
(cd bench && cargo build --release)       # bench: footprints, queue replay, relations

# 1. How each project merges (merge queue, bors, Tide, label bot, plain merges).
python3 survey/survey.py owner/name ...    # -> survey/out/

# 2. Clone each repository in repos.json into repos/<owner>__<name>. History since
#    2026-07-10 is enough (git clone --shallow-since=2026-07-10), except where landings are
#    merge commits: gradle, kubernetes, prometheus and rust-analyzer were cloned in full,
#    because a shallow boundary breaks their first-parent chain.
# 3. Merged pull requests, Actions runs and first-parent landings.
python3 collect.py $(python3 -c "import json;print(*json.load(open('repos.json')))")
python3 gradle_statuses.py                 # gradle's TeamCity statuses (CI outside Actions)

# 4. Exact footprints for every landing, then one per rebase-merged pull request.
./footprints.sh
python3 footprints_folded.py

# 5. Analyses.
python3 queues.py rust-lang/rust-analyzer junit-team/junit-framework trinodb/trino \
  bevyengine/bevy gradle/gradle rust-lang/rust traefik/traefik kubernetes/kubernetes
python3 breaks.py astral-sh/ruff tokio-rs/tokio nushell/nushell prometheus/prometheus \
  gohugoio/hugo vitejs/vite vitest-dev/vitest storybookjs/storybook microsoft/vscode \
  home-assistant/core fastapi/fastapi pydantic/pydantic python/cpython keycloak/keycloak \
  square/okhttp
python3 escapes.py && python3 escapes_diff.py && python3 classify.py
python3 roi_assumption.py                  # the ROI report's old assumption against the real bases
python3 summary.py && python3 tables.py > results/tables.md && python3 fill_docs.py
./pdf/build.sh                              # site/assets/sheltie-on-real-repositories.pdf
```

`classify.py` holds the `class` of each escape candidate, assigned by reading its failed jobs,
follow-up commits and changed files. A candidate it has no label for is reported as
unclassified instead of being guessed.

## Files

| File | What it does |
| --- | --- |
| `repos.json` | The projects: merge setup (`kind`), adapter (`tool`), branch, and what is known about their queue CI |
| `survey/survey.py` | Classifies how a repository merges, from rulesets, Actions events and pull request timelines |
| `collect.py` | Merged pull requests (GraphQL: check suites, merge-queue and label events, bors comments), Actions runs, landings |
| `gradle_statuses.py` | Commit statuses on gradle's landed merge-queue commits, for its TeamCity queue CI time |
| `footprints.sh`, `footprints_folded.py` | Exact footprints with the released adapters (`bench footprints`) |
| `bench/` | Rust: `footprints`, `queue` (replay of real arrivals through the Sheltie scheduler), `relate` |
| `queues.py` | Queue projects: real waits, builds and ejections against Sheltie on the same entries |
| `breaks.py` | Plain-merge projects: stale merges, Sheltie's rebuilds, red `main` episodes and a chance baseline |
| `escapes.py`, `escapes_diff.py` | Queue failures on pull requests Sheltie would land at once: failed jobs, follow-up commits, whether the diff changed |
| `roi_assumption.py` | The same replay with and without each change's real base (`results/roi-assumption.json`) |
| `summary.py`, `tables.py` | `results/summary.json` and the Markdown tables |
| `fill_docs.py`, `pdf/build.sh` | The tables into `docs/real-world.md`, and its PDF |

## How the replay is checked

`bench queue` uses a copy of `sheltie_scheduler::roi::replay` that also returns each change's
wait and decision. On every run it replays the same input through the shipped function and
fails if the totals or decisions differ.
