"""Markdown tables for the write-up, generated from results/*.json (no hand-copied numbers).

usage: python3 tables.py > results/tables.md
"""
import collections, glob, json, os
from common import HERE, hm, load

R = os.path.join(HERE, "results")
S = json.load(open(os.path.join(R, "summary.json")))
CONF = json.load(open(os.path.join(HERE, "repos.json")))
NAMES = {"home-assistant/core": "home-assistant", "rust-lang/rust": "rust-lang/rust"}
name = lambda r: NAMES.get(r, r.split("/")[1])
pc = lambda a, b: f"{100 * a / b:.0f}%" if b else "–"

QUEUE = ["rust-lang/rust-analyzer", "junit-team/junit-framework", "trinodb/trino", "bevyengine/bevy",
         "gradle/gradle", "rust-lang/rust", "traefik/traefik"]
Q = {q["repo"]: json.load(open(os.path.join(R, f"queue-{q['repo'].replace('/', '__')}.json"))) for q in S["queue"]}
KIND = {"mq": "GitHub merge queue", "bors": "bors", "label": "label bot (serial)", "tide": "Prow/Tide"}

print("## queue-setup\n")
print("| Project | Queue | Queue CI vs PR CI | Evidence |\n| --- | --- | --- | --- |")
for r in QUEUE + ["kubernetes/kubernetes"]:
    q = Q[r]
    print(f"| {name(r)} | {KIND[q['kind']]} | {q['queue_ci']} | {q['queue_ci_evidence']} |")

print("\n## queue-like-for-like\n")
print("| Project | CI per build | PRs (never ejected) | Real wait: median / p90 / total | Sheltie wait: median / p90 / total | Total wait saved |")
print("| --- | --- | --- | --- | --- | --- |")
for r in QUEUE:
    q, c = Q[r], Q[r]["clean"]
    saved = 1 - c["sheltie_total"] / c["observed_total"] if c["observed_total"] else 0
    print(f"| {name(r)} | {hm(q['ci']['median_seconds'])} | {c['n']} | {hm(c['observed_median'])} / {hm(c['observed_p90'])} / {hm(c['observed_total'])} "
          f"| {hm(c['sheltie_median'])} / {hm(c['sheltie_p90'])} / {hm(c['sheltie_total'])} | {saved * 100:.0f}% |")

print("\n## queue-ci-cost\n")
print("| Project | Queue CI | Real queue builds (failed) | Sheltie CI runs | Sheltie, if PR CI had to run the queue's full CI |")
print("| --- | --- | --- | --- | --- |")
for r in QUEUE:
    q = Q[r]
    s = q["sheltie"]["replay"]["sheltie"]["ci_runs"]
    full = q.get("sheltie_ci_runs_with_full_pr_ci")
    b = q["ci"]["builds"]
    failed = q["ci"]["failed_builds"]
    bs = f"{'≥' if r == 'gradle/gradle' else ''}{b}" + (f" ({failed})" if failed is not None else "") if b is not None else "–"
    print(f"| {name(r)} | {q['queue_ci']} | {bs} | {s} | {full if full is not None else 'not needed'} |")

print("\n## queue-decisions\n")
print("| Project | Changes | Land at once | Rebuild (reverify) | Merge engine | Built together | Leaky footprints |")
print("| --- | --- | --- | --- | --- | --- | --- |")
for r in QUEUE:
    q = Q[r]; d = q["sheltie"]["replay"]["decisions"]; n = q["landings"]
    print(f"| {name(r)} | {n} | {d['landed_at_once']} ({pc(d['landed_at_once'], n)}) | {d['reverified']} | {d['merge_engine']} | {d['built_together']} | {q['sheltie']['leaky']} ({pc(q['sheltie']['leaky'], n)}) |")

print("\n## queue-failures-by-decision\n")
print("| Project | Failed in queue, Sheltie lands at once | Failed in queue, Sheltie rebuilds or merges |")
print("| --- | --- | --- |")
for r in ["trinodb/trino", "gradle/gradle", "bevyengine/bevy", "junit-team/junit-framework", "rust-lang/rust-analyzer"]:
    q = Q[r]; L = load(r)[0]
    dec = {l["sha"]: d for l, (w, d) in zip(L, q["sheltie"]["per_change"])}
    c = collections.defaultdict(lambda: [0, 0])
    for row in q["rows"]:
        k = "now" if dec[row["landing"]] == "land_now" else "other"
        c[k][0] += 1
        c[k][1] += any(x in ("failed_checks", "checks_timed_out") for x in row["ejected"])
    print(f"| {name(r)} | {c['now'][1]} / {c['now'][0]} ({pc(c['now'][1], c['now'][0])}) | {c['other'][1]} / {c['other'][0]} ({pc(c['other'][1], c['other'][0])}) |")

E = json.load(open(os.path.join(R, "escapes.json")))
print("\n## escapes\n")
print("| Outcome after the queue failure | " + " | ".join(name(r) for r in ["trinodb/trino", "gradle/gradle", "bevyengine/bevy", "junit-team/junit-framework", "rust-lang/rust-analyzer"]) + " | Total |")
print("| --- " * 7 + "|")
classes = sorted({e["class"] for e in E}, key=lambda c: (c.startswith("diff unchanged"), c))
for cl in classes:
    row = [sum(1 for e in E if e["class"] == cl and e["repo"] == r) for r in ["trinodb/trino", "gradle/gradle", "bevyengine/bevy", "junit-team/junit-framework", "rust-lang/rust-analyzer"]]
    print(f"| {cl} | " + " | ".join(str(x) for x in row) + f" | {sum(row)} |")
print(f"| **All candidates** | " + " | ".join(str(sum(1 for e in E if e['repo'] == r)) for r in ["trinodb/trino", "gradle/gradle", "bevyengine/bevy", "junit-team/junit-framework", "rust-lang/rust-analyzer"]) + f" | {len(E)} |")

print("\n## plain\n")
print("| Project | Adapter | PRs | PR CI | Stale at merge | Sheltie would rebuild | Extra CI (wall time, upper bound) | Leaky footprints | Persistent red episodes | …with an overlapping landing: seen vs by chance |")
print("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |")
O = C = P = 0
for b in sorted(S["breaks"], key=lambda b: (b["tool"], b["repo"])):
    nm = name(b["repo"])
    ci_ok = b["repo"] != "storybookjs/storybook"
    o = b["verdicts"].get("stale base, overlapping landing: Sheltie rebuilds before landing", 0)
    if b["post_merge_ci"]:
        O += o; C += b["overlap_by_chance"]; P += b["persistent_red"]
    print(f"| {nm} | {b['tool']} | {b['pr_landings']} | {hm(b['ci_seconds_median']) if ci_ok else 'n/m'} | {b['stale']}/{b['tested']} ({pc(b['stale'], b['tested'])}) "
          f"| {b['sheltie_rebuilds']} ({pc(b['sheltie_rebuilds'], b['tested'])}) | {hm(b['sheltie_extra_ci_seconds']) if ci_ok else 'n/m'} | {b['leaky']} ({pc(b['leaky'], b['tested'])}) "
          f"| {b['persistent_red'] if b['post_merge_ci'] else 'n/m'} | {(str(o) + ' vs ' + format(b['overlap_by_chance'], '.1f')) if b['post_merge_ci'] else 'n/m'} |")
print(f"| **Total** | | | | | | | | **{P}** | **{O} vs {C:.1f}** |")

print("\n## verdicts\n")
V = collections.Counter()
for b in S["breaks"]:
    if b["post_merge_ci"]:
        V.update(b["verdicts"])
for k, v in V.most_common():
    print(f"- {k}: {v}")

print("\n## leaky\n")
print("| Project | Adapter | Leaky footprints | Most common reason |\n| --- | --- | --- | --- |")
for d in sorted(os.listdir(os.path.join(HERE, "fp"))):
    repo = d.replace("__", "/", 1)
    cache = os.path.join(HERE, "fp-folded" if CONF[repo].get("rebase") else "fp", d)
    n = lk = 0
    reasons = collections.Counter()
    for f in os.listdir(cache):
        v = json.load(open(os.path.join(cache, f)))
        if "error" in v:
            continue
        n += 1
        if v["footprint"]["leaky"]:
            lk += 1
            for x in set(r.split(": ", 1)[-1] for r in v.get("leak_reasons", [])):
                reasons[x] += 1
    top = reasons.most_common(1)[0][0] if reasons else "–"
    print(f"| {name(repo)} | {CONF[repo]['tool']} | {lk}/{n} ({pc(lk, n)}) | {top if len(top) <= 80 else top[:77] + '…'} |")
