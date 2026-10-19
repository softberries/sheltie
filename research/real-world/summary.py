"""One table per group from results/*.json -> results/summary.json (and a text dump)."""
import json, os, glob
from common import HERE, hm

R = os.path.join(HERE, "results")
queue, breaks = [], []
for f in sorted(glob.glob(os.path.join(R, "queue-*.json"))):
    q = json.load(open(f))
    s, sv = q["sheltie"]["replay"]["sheltie"], q["sheltie"]["replay"]
    queue.append({
        "repo": q["repo"], "kind": q["kind"], "tool": q["tool"], "landings": q["landings"], "queued": q["queued"],
        "ci_source": q["ci"]["source"], "ci_seconds": q["ci"]["median_seconds"],
        "observed_median": q["observed"]["median"], "observed_p90": q["observed"]["p90"],
        "observed_total": q["observed"]["total_all"],
        "queue_builds": q["ci"].get("builds"), "failed_builds": q["ci"].get("failed_builds"),
        "ejections": q["observed"]["ejections"], "ejection_reasons": q["observed"]["ejection_reasons"],
        "sheltie_median": s["median_wait_seconds"], "sheltie_p90": s["p90_wait_seconds"],
        "sheltie_total": s["total_wait_seconds"], "sheltie_ci_runs": s["ci_runs"],
        "decisions": sv["decisions"], "leaky": q["sheltie"]["leaky"], "missing": q["sheltie"]["missing"],
    })
for f in sorted(glob.glob(os.path.join(R, "breaks-*.json"))):
    b = json.load(open(f))
    breaks.append({k: v for k, v in b.items() if k != "episodes"})
json.dump({"queue": queue, "breaks": breaks}, open(os.path.join(R, "summary.json"), "w"), indent=1)

for q in queue:
    print(f"{q['repo']:32} {q['kind']:5} CI {hm(q['ci_seconds']):>10} | real med {hm(q['observed_median']):>10} p90 {hm(q['observed_p90']):>12} "
          f"total {hm(q['observed_total']):>10} builds {q['queue_builds']} fail {q['failed_builds']} ej {q['ejections']} | "
          f"Sheltie med {hm(q['sheltie_median'])} p90 {hm(q['sheltie_p90'])} total {hm(q['sheltie_total'])} runs {q['sheltie_ci_runs']} "
          f"leaky {q['leaky']}/{q['landings']} missing {q['missing']}")
for b in breaks:
    print(f"{b['repo']:32} {b['tool']:6} PRs {b['pr_landings']} direct {b['direct_pushes']} CI {hm(b['ci_seconds_median'])} "
          f"post-merge CI {b['post_merge_ci']} | stale {b['stale']}/{b['tested']} rebuilds {b['sheltie_rebuilds']} (+{hm(b['sheltie_extra_ci_seconds'])}) "
          f"leaky {b['leaky']} | red {b['red_episodes']} persistent {b['persistent_red']} ({hm(b['persistent_red_seconds'])}) {b['verdicts']}")
