"""Queue projects: the real queue (time in queue, queue builds, ejections) against Sheltie
replaying the same queue entries.

usage: python3 queues.py owner/name   -> results/queue-<owner__name>.json
"""
import json, os, re, sys
from common import *


def ready_time(conf, pr, merged):
    """When the pull request entered the merge process, or None if it bypassed it."""
    kind = conf["kind"]
    tl = [n for n in pr["timelineItems"]["nodes"] if n.get("createdAt") and t(n["createdAt"]) <= merged]
    if kind == "mq":
        adds = [t(n["createdAt"]) for n in tl if n["__typename"] == "AddedToMergeQueueEvent"]
        return max(adds) if adds else None
    if kind == "label":
        adds = [t(n["createdAt"]) for n in tl if n["__typename"] == "LabeledEvent" and n["label"]["name"] == conf["label"]]
        return max(adds) if adds else None
    if kind == "tide":
        lab = lambda name: [t(n["createdAt"]) for n in tl if n["__typename"] == "LabeledEvent" and n["label"]["name"] == name]
        lgtm, appr = lab("lgtm"), lab("approved")
        if not lgtm or not appr:
            return None
        st = (((pr["commits"]["nodes"] or [{}])[0].get("commit") or {}).get("status") or {}).get("contexts") or []
        tests = [t(c["createdAt"]) for c in st if c["context"].startswith("pull-") and c["state"] == "SUCCESS" and t(c["createdAt"]) <= merged]
        # Tide also waits for blocking labels (holds, needs-rebase, ...) to be removed.
        unblocked = [t(n["createdAt"]) for n in tl if n["__typename"] == "UnlabeledEvent"
                     and n["label"]["name"].startswith(("do-not-merge/", "needs-"))]
        return max([max(lgtm), max(appr)] + ([max(tests)] if tests else []) + ([max(unblocked)] if unblocked else []))
    if kind == "bors":
        cs = [c for c in pr["comments"]["nodes"] if (c.get("author") or {}).get("login", "").endswith("bors") and t(c["createdAt"]) <= merged]
        appr = [t(c["createdAt"]) for c in cs if "has been approved by" in c["bodyText"]]
        return max(appr) if appr else None
    raise ValueError(kind)


def tested_time(conf, pr, merged):
    """When the pull request's own CI last started: the main it was tested against."""
    if conf["kind"] == "tide":
        st = (((pr["commits"]["nodes"] or [{}])[0].get("commit") or {}).get("status") or {}).get("contexts") or []
        # Only the latest status per context is visible; its time is when that job finished,
        # so the earliest of them approximates when the presubmits last ran.
        ts = [t(c["createdAt"]) for c in st if c["context"].startswith("pull-") and t(c["createdAt"]) <= merged]
        return min(ts) if ts else None
    ss = [s for s in suites(pr) if t(s["createdAt"]) <= merged]
    return max(t(s["createdAt"]) for s in ss) if ss else None


def bors_stats(prs):
    durations, tests = [], 0
    for p in prs:
        for c in p["comments"]["nodes"]:
            if not (c.get("author") or {}).get("login", "").endswith("bors"):
                continue
            tests += "Testing commit" in c["bodyText"]
            m = re.search(r"Duration: (?:(\d+)h )?(?:(\d+)m )?(\d+)s", c["bodyText"])
            if "Test successful" in c["bodyText"] and m:
                durations.append(int(m.group(1) or 0) * 3600 + int(m.group(2) or 0) * 60 + int(m.group(3)))
    return durations, tests


def merge_group_builds(runs):
    """Queue builds: merge_group runs grouped by commit; duration from first start to last finish."""
    by = {}
    for r in runs:
        if r["event"] == "merge_group" and r["run_started_at"]:
            by.setdefault(r["head_sha"], []).append(r)
    builds = []
    for sha, rs in by.items():
        start = min(t(r["run_started_at"]) for r in rs)
        end = max(t(r["updated_at"]) for r in rs)
        concl = [r["conclusion"] for r in rs]
        # Non-required workflows may fail without ejecting (bevy), so a build only counts as
        # finished if nothing in it was cancelled; failures come from the ejection events.
        ok = "cancelled" not in concl
        bad = any(c == "failure" for c in concl)
        builds.append({"sha": sha, "seconds": (end - start).total_seconds(), "ok": ok, "failed": bad})
    return builds


def pr_ci_seconds(prs):
    out = []
    for p in prs:
        ss = suites(p)
        if ss:
            out.append((max(t(s["updatedAt"]) for s in ss) - min(t(s["createdAt"]) for s in ss)).total_seconds())
    return out


def main(repo):
    conf = CONF[repo]
    landings, prs, runs = load(repo)
    lmap = pr_landing_map(landings, prs)
    rows, bypass = [], 0
    ready_of, landed_of, based_of = {}, {}, {}
    for p in prs:
        l = lmap.get(p["number"])
        if not l:
            continue
        merged = t(p["mergedAt"])
        r = ready_time(conf, p, merged)
        if r is None:
            bypass += 1
            continue
        ready_of[l["sha"]] = r
        landed_of[l["sha"]] = merged
        tb = tested_time(conf, p, merged)
        if tb is not None:
            based_of[l["sha"]] = min(tb, r)
        removed = [n.get("reason") for n in p["timelineItems"]["nodes"] if n["__typename"] == "RemovedFromMergeQueueEvent"]
        rows.append({"pr": p["number"], "landing": l["sha"], "ready": iso(r), "landed": iso(merged),
                     "wait": (merged - r).total_seconds(),
                     "ejected": [x for x in removed if x != "merged"]})
    waits = [r["wait"] for r in rows]

    # Queue CI: duration per build, builds and failures.
    ci = {"source": None}
    if conf["kind"] == "mq" and conf.get("actions", True):
        b = merge_group_builds(runs)
        okd = [x["seconds"] for x in b if x["ok"]]
        bad = sum(1 for p in prs for n in p["timelineItems"]["nodes"]
                  if n["__typename"] == "RemovedFromMergeQueueEvent" and n.get("reason") in ("failed_checks", "checks_timed_out"))
        ci = {"source": "merge_group runs", "builds": len(b), "failed_builds": bad,
              "median_seconds": median(okd)}
    elif conf["kind"] == "bors":
        d, tests = bors_stats(prs)
        ci = {"source": "bors comments", "builds": tests, "failed_builds": tests - len(d), "median_seconds": median(d)}
    elif conf["kind"] == "label":
        d = pr_ci_seconds(prs)
        ci = {"source": "pull request CI duration (the bot rebases, then waits for CI)", "builds": len(rows), "failed_builds": None,
              "median_seconds": median(d)}
    elif conf["kind"] == "mq":
        # CI outside Actions (gradle: TeamCity). Duration: the gating status on each landed
        # merge-queue commit, first pending to final state. Every queue entry gets at least
        # one build.
        adds = sum(1 for p in prs for n in p["timelineItems"]["nodes"] if n["__typename"] == "AddedToMergeQueueEvent")
        bad = sum(1 for p in prs for n in p["timelineItems"]["nodes"]
                  if n["__typename"] == "RemovedFromMergeQueueEvent" and n.get("reason") in ("failed_checks", "checks_timed_out"))
        st = json.load(open(os.path.join(HERE, "data", repo.replace("/", "__"), "statuses.json")))
        gate = conf["gate"]
        d = []
        for ss in st.values():
            p = [t(x["created_at"]) for x in ss if x["context"] == gate and x["state"] == "pending"]
            f = [t(x["created_at"]) for x in ss if x["context"] == gate and x["state"] != "pending"]
            if p and f:
                d.append((max(f) - min(p)).total_seconds())
        ci = {"source": f"commit status '{gate}' on landed merge-queue commits; builds = queue entries (lower bound)",
              "builds": adds, "failed_builds": bad, "median_seconds": median(d)}
    else:
        ci = {"source": "estimated: 10th percentile of time in queue (CI not on GitHub Actions)", "builds": None,
              "failed_builds": None, "median_seconds": pct(waits, 10)}

    # Sheltie on the same arrivals: every landing, ready when it entered the queue (or when it landed).
    changes = [{"sha": l["sha"], "ready_at": iso(ready_of.get(l["sha"], t(l["at"]))),
                "based_at": iso(based_of.get(l["sha"], ready_of.get(l["sha"], t(l["at"]))))} for l in landings]
    sim = queue_replay(repo, ci["median_seconds"], changes)
    # Like for like: pull requests the real queue never ejected (the replay assumes CI passes).
    per = {l["sha"]: w for l, (w, _) in zip(landings, sim["per_change"])}
    clean = [r for r in rows if not r["ejected"]]
    clean_obs = [r["wait"] for r in clean]
    clean_sim = [per[r["landing"]] for r in clean if r["landing"] in per]
    observed_all = [(landed_of.get(l["sha"], t(l["at"])) - ready_of.get(l["sha"], t(l["at"]))).total_seconds() for l in landings]
    res = {
        "repo": repo, "kind": conf["kind"], "tool": conf["tool"], "landings": len(landings), "queued": len(rows),
        "bypassed_or_unmatched": len(landings) - len(rows), "ci": ci, "with_tested_time": len(based_of),
        "observed": {"median": median(waits), "p90": pct(waits, 90), "total_all": sum(observed_all),
                     "median_all": median(observed_all), "p90_all": pct(observed_all, 90),
                     "ejections": sum(len(r["ejected"]) for r in rows),
                     "ejection_reasons": {k: sum(r["ejected"].count(k) for r in rows) for k in {x for r in rows for x in r["ejected"]}}},
        "clean": {"n": len(clean), "observed_median": median(clean_obs), "observed_p90": pct(clean_obs, 90),
                  "observed_total": sum(clean_obs), "sheltie_median": median(clean_sim), "sheltie_p90": pct(clean_sim, 90),
                  "sheltie_total": sum(clean_sim)},
        "queue_ci": conf.get("queue_ci"), "queue_ci_evidence": conf.get("queue_ci_evidence"),
        # Where the queue tests more than PR CI, Sheltie would need that full CI on every PR
        # before landing: one full run per change, plus its rebuilds.
        "sheltie_ci_runs_with_full_pr_ci": len(landings) + sim["replay"]["sheltie"]["ci_runs"] if conf.get("queue_ci") == "heavier" else None,
        "sheltie": sim, "rows": rows, "replay_input": changes,
    }
    os.makedirs(os.path.join(HERE, "results"), exist_ok=True)
    json.dump(res, open(os.path.join(HERE, "results", f"queue-{repo.replace('/', '__')}.json"), "w"), indent=1)
    s = sim["replay"]["sheltie"]
    print(f"{repo}: {len(rows)}/{len(landings)} queued; CI {hm(ci['median_seconds'])} ({ci['source']}); "
          f"observed wait median {hm(res['observed']['median'])} p90 {hm(res['observed']['p90'])} total {hm(res['observed']['total_all'])}; "
          f"queue builds {ci.get('builds')} (failed {ci.get('failed_builds')}), ejections {res['observed']['ejections']} | "
          f"Sheltie median {hm(s['median_wait_seconds'])} p90 {hm(s['p90_wait_seconds'])} total {hm(s['total_wait_seconds'])}, "
          f"CI runs {s['ci_runs']} | never-ejected {res['clean']['n']}: real {hm(res['clean']['observed_median'])}/{hm(res['clean']['observed_p90'])}/{hm(res['clean']['observed_total'])} "
          f"Sheltie {hm(res['clean']['sheltie_median'])}/{hm(res['clean']['sheltie_p90'])}/{hm(res['clean']['sheltie_total'])}, CI runs, decisions {sim['replay']['decisions']}, missing fp {sim['missing']}, leaky {sim['leaky']}")


if __name__ == "__main__":
    for r in sys.argv[1:]:
        main(r)
