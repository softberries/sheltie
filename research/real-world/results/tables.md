## queue-setup

| Project | Queue | Queue CI vs PR CI | Evidence |
| --- | --- | --- | --- |
| rust-analyzer | GitHub merge queue | same | same workflows on pull_request and merge_group, no merge_group-only jobs |
| junit-framework | GitHub merge queue | same | same workflows on pull_request and merge_group, no merge_group-only jobs |
| trino | GitHub merge queue | same | ci.yml: merge_group differs only by a checkout step for path filtering |
| bevy | GitHub merge queue | heavier | validation-jobs.yml: jobs with if: github.event_name == merge_group (iOS, Android, wasm, examples) |
| gradle | GitHub merge queue | heavier | PR commits get Quick Feedback (Linux) and unit tests; queue commits get the full TeamCity pipeline (integration, distribution, performance tests) |
| rust-lang/rust | bors | heavier | src/ci/github-actions/jobs.yml: 11 pr jobs, 82 auto (bors) jobs |
| traefik | label bot (serial) | same | the bot rebases and waits for the same PR CI |
| kubernetes | Prow/Tide | unknown | Prow; not visible on GitHub |

## queue-like-for-like

| Project | CI per build | PRs (never ejected) | Real wait: median / p90 / total | Sheltie wait: median / p90 / total | Total wait saved |
| --- | --- | --- | --- | --- | --- |
| rust-analyzer | 8 min | 279 | 9 min / 10 min / 43 h 35 min | 0 s / 8 min / 13 h 57 min | 68% |
| junit-framework | 9 min | 147 | 9 min / 18 min / 27 h 39 min | 0 s / 9 min / 6 h 57 min | 75% |
| trino | 53 min | 453 | 56 min / 1 h 55 min / 22 d 3 h | 0 s / 1 h 21 min / 9 d 0 h | 59% |
| bevy | 35 min | 511 | 38 min / 1 h 26 min / 17 d 18 h | 34 min / 1 h 7 min / 9 d 10 h | 47% |
| gradle | 1 h 19 min | 297 | 1 h 32 min / 2 h 51 min / 21 d 21 h | 0 s / 2 h 24 min / 9 d 0 h | 59% |
| rust-lang/rust | 3 h 8 min | 428 | 5 h 18 min / 17 h 2 min / 155 d 23 h | 0 s / 3 h 8 min / 19 d 8 h | 88% |
| traefik | 13 min | 37 | 2 min / 15 min / 9 h 37 min | 0 s / 0 s / 0 s | 100% |

## queue-ci-cost

| Project | Queue CI | Real queue builds (failed) | Sheltie CI runs | Sheltie, if PR CI had to run the queue's full CI |
| --- | --- | --- | --- | --- |
| rust-analyzer | same | 297 (7) | 83 | *not needed* |
| junit-framework | same | 165 (5) | 36 | *not needed* |
| trino | same | 2357 (579) | 348 | *not needed* |
| bevy | heavier | 768 (71) | 224 | 816 |
| gradle | heavier | ≥599 (167) | 170 | 580 |
| rust-lang/rust | heavier | 625 (190) | 116 | 556 |
| traefik | same | 37 | 0 | *not needed* |

## queue-decisions

| Project | Changes | Land at once | Rebuild (reverify) | Merge engine | Built together | Leaky footprints |
| --- | --- | --- | --- | --- | --- | --- |
| rust-analyzer | 289 | 203 (70%) | 35 | 46 | 5 | 160 (55%) |
| junit-framework | 228 | 191 (84%) | 3 | 34 | 0 | 24 (11%) |
| trino | 746 | 391 (52%) | 86 | 261 | 8 | 395 (53%) |
| bevy | 592 | 328 (55%) | 92 | 115 | 57 | 282 (48%) |
| gradle | 410 | 239 (58%) | 30 | 139 | 2 | 129 (31%) |
| rust-lang/rust | 440 | 318 (72%) | 74 | 38 | 10 | 417 (95%) |
| traefik | 37 | 37 (100%) | 0 | 0 | 0 | 25 (68%) |

## queue-failures-by-decision

| Project | Failed in queue, Sheltie lands at once | Failed in queue, Sheltie rebuilds or merges |
| --- | --- | --- |
| trino | 112 / 388 (29%) | 169 / 355 (48%) |
| gradle | 58 / 239 (24%) | 40 / 171 (23%) |
| bevy | 44 / 328 (13%) | 14 / 264 (5%) |
| junit-framework | 5 / 117 (4%) | 0 / 35 (0%) |
| rust-analyzer | 4 / 203 (2%) | 2 / 86 (2%) |

## escapes

| Outcome after the queue failure | trino | gradle | bevy | junit-framework | rust-analyzer | Total |
| --- | --- | --- | --- | --- | --- | --- |
| change to CI workflow, only exercised in the queue | 2 | 0 | 0 | 0 | 0 | 2 |
| code change after a PR-CI job failed in the queue; cause not determined | 0 | 0 | 1 | 0 | 0 | 1 |
| code change; failing job and cause not determined | 5 | 0 | 0 | 0 | 2 | 7 |
| failing jobs run only in the queue (wasm, iOS/Android, examples, unused deps) | 0 | 0 | 4 | 0 | 0 | 4 |
| repository-wide check (distribution size, API list, version list, performance baseline) | 0 | 10 | 0 | 0 | 0 | 10 |
| semantic conflict with main ("Fix example merge conflict") | 0 | 0 | 1 | 0 | 0 | 1 |
| test fix after a PR-CI job failed in the queue; cause not determined | 0 | 0 | 1 | 0 | 0 | 1 |
| test or code fix; PR CI is a subset of queue CI, failing test not identified | 0 | 14 | 0 | 0 | 0 | 14 |
| diff unchanged: rebase or merge of main only | 5 | 4 | 8 | 1 | 0 | 18 |
| **All candidates** | 12 | 28 | 15 | 1 | 2 | 58 |

## plain

| Project | Adapter | PRs | PR CI | Stale at merge | Sheltie would rebuild | Extra CI (wall time, upper bound) | Leaky footprints | Persistent red episodes | …with an overlapping landing: seen vs by chance |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| ruff | cargo | 1388 | 12 min | 726/1295 (56%) | 440 (34%) | 3 d 23 h | 790 (61%) | 6 | 2 vs 2.4 |
| nushell | cargo | 364 | 23 min | 186/346 (54%) | 87 (25%) | 33 h 21 min | 179 (52%) | 0 | 0 vs 0.0 |
| tokio | cargo | 163 | 1 h 16 min | 75/162 (46%) | 36 (22%) | 45 h 38 min | 8 (5%) | 1 | 0 vs 0.2 |
| hugo | go | 144 | 1 h 5 min | 81/143 (57%) | 64 (45%) | 2 d 21 h | 21 (15%) | 5 | 1 vs 2.2 |
| prometheus | go | 414 | 28 min | 325/414 (79%) | 220 (53%) | 4 d 6 h | 95 (23%) | 20 | 12 vs 10.6 |
| okhttp | gradle | 147 | 29 min | 21/38 (55%) | 14 (37%) | 6 h 59 min | 6 (16%) | 2 | 2 vs 0.7 |
| keycloak | maven | 844 | 59 min | 758/840 (90%) | 559 (67%) | 23 d 4 h | 515 (61%) | n/m | n/m |
| vscode | node | 5221 | 8 min | 4675/5216 (90%) | 4550 (87%) | 28 d 4 h | 5110 (98%) | 11 | 10 vs 9.6 |
| storybook | node | 142 | n/m | 89/125 (71%) | 74 (59%) | n/m | 8 (6%) | n/m | n/m |
| vite | node | 321 | 6 min | 162/317 (51%) | 98 (31%) | 11 h 9 min | 36 (11%) | 21 | 7 vs 6.5 |
| vitest | node | 370 | 15 min | 76/367 (21%) | 57 (16%) | 14 h 19 min | 19 (5%) | 30 | 2 vs 6.0 |
| fastapi | python | 178 | 5 min | 57/178 (32%) | 11 (6%) | 1 h 3 min | 10 (6%) | 1 | 0 vs 0.1 |
| home-assistant | python | 6293 | 4 min | 5660/6291 (90%) | 2068 (33%) | 6 d 3 h | 3029 (48%) | 52 | 34 vs 33.6 |
| pydantic | python | 220 | 12 min | 77/220 (35%) | 55 (25%) | 11 h 16 min | 54 (25%) | 5 | 2 vs 1.2 |
| cpython | python | 1211 | 32 min | 919/1211 (76%) | 691 (57%) | 15 d 10 h | 914 (75%) | 22 | 11 vs 12.6 |
| **Total** | | | | | | | | **176** | **83 vs 85.8** |

## verdicts

- stale base, overlapping landing: Sheltie rebuilds before landing: 83
- tested on the current main: not an interaction: 64
- stale base, no overlap: Sheltie lands it too: 27
- direct push or untested change in range: 2

## leaky

| Project | Adapter | Leaky footprints | Most common reason |
| --- | --- | --- | --- |
| ruff | cargo | 858/1416 (61%) | build script crates/ty/build.rs |
| bevy | cargo | 282/592 (48%) | included file ci_testing_config.ron not in tree |
| fastapi | python | 10/326 (3%) | scripts/translate.py is outside the module graph |
| hugo | go | 35/182 (19%) | testscripts/commands/no_symlinks.txt is outside the module graph |
| gradle | gradle | 129/410 (31%) | released-versions.json is outside the module graph |
| home-assistant | python | 3029/6298 (48%) | setup.py builds extensions |
| junit-framework | gradle | 24/228 (11%) | documentation/.tool-versions is outside the module graph |
| keycloak | maven | 517/846 (61%) | build plugin runs code (maven-antrun-plugin) |
| kubernetes | go | 271/925 (29%) | test/compatibility_lifecycle/reference/versioned_feature_list.yaml is outside… |
| vscode | node | 5172/5282 (98%) | install script |
| nushell | cargo | 195/368 (53%) | build script scripts/build.rs |
| prometheus | go | 95/415 (23%) | web/ui/pnpm-lock.yaml is outside the module graph |
| pydantic | python | 54/221 (24%) | pydantic-core/src/serializers/shared.rs is outside the module graph |
| cpython | python | 917/1214 (76%) | setup.py builds extensions |
| rust-lang/rust | cargo | 417/440 (95%) | cannot parse compiler/rustc_trait_selection/src/error_reporting/infer/need_ty… |
| rust-analyzer | cargo | 160/289 (55%) | build script crates/rust-analyzer/build.rs |
| okhttp | gradle | 9/153 (6%) | okhttp/api/android/okhttp.api is outside the module graph |
| storybook | node | 18/162 (11%) | scripts/build/entry-configs.ts is outside the module graph |
| tokio | cargo | 8/166 (5%) | proc-macro crate |
| traefik | go | 25/37 (68%) | integration/gateway-api-conformance-reports/v1.6.1/experimental-v3.8-default-… |
| trino | maven | 395/746 (53%) | build plugin runs code (exec-maven-plugin) |
| vite | node | 37/328 (11%) | vitest.config.e2e.ts is outside the module graph |
| vitest | node | 19/370 (5%) | knip.jsonc is outside the module graph |
