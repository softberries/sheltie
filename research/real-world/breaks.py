"""Plain-merge projects: how often main went red after merges, whether the culprit was tested
on a stale base with an overlapping landing in between (Sheltie would have rebuilt it on the
new head before landing), and what Sheltie's extra builds would have cost.

usage: python3 -I breaks.py owner/name   -> results/breaks-<owner__name>.json
"""
import json, os, re, sys
from common import *

GOOD, BAD = ("success",), ("failure", "timed_out", "startup_failure")


def main(repo):
    conf = CONF[repo]
    landings, prs, runs = load(repo)
    idx = {l["sha"]: i for i, l in enumerate(landings)}
    lmap = pr_landing_map(landings, prs)
    pr_of = {l["sha"]: n for n, l in lmap.items()}
    prs_by = {p["number"]: p for p in prs}

    # The CI: workflows that run both on pull requests and on pushes to the branch.
    push = [r for r in runs if r["event"] == "push" and r["head_branch"] == conf["branch"] and r["head_sha"] in idx]
    pr_names = {s["workflowRun"]["workflow"]["name"] for p in prs for s in suites(p)}
    # Not build or test: they fail on new advisories, releases or tokens, not on merges.
    nontest = re.compile(r"audit|zizmor|codeql|copilot|release|publish|^CR$|typos|cla|labeler|stale", re.I)
    ci_names = sorted(n for n in {r["name"] for r in push} & pr_names if not nontest.search(n))

    # Latest attempt of each CI workflow per landing.
    latest = {}
    for r in push:
        if r["name"] not in ci_names:
            continue
        k = (r["head_sha"], r["name"])
        if k not in latest or (r["run_attempt"], r["created_at"]) > (latest[k]["run_attempt"], latest[k]["created_at"]):
            latest[k] = r
    flaky_reruns = sum(1 for r in latest.values() if r["run_attempt"] > 1 and r["conclusion"] in GOOD)
    pipeline = {}
    for (sha, _), r in latest.items():
        if r["conclusion"] in GOOD and r["run_started_at"]:
            d = (t(r["updated_at"]) - t(r["run_started_at"])).total_seconds()
            pipeline[sha] = max(pipeline.get(sha, 0), d)
    ci_seconds = median(list(pipeline.values()))
    # No CI on the branch (e.g. keycloak): breakage is not visible, but staleness and Sheltie's
    # rebuild cost still are, from the pull requests' own CI.
    tested_names = ci_names or sorted(n for n in pr_names if not nontest.search(n))
    # What a Sheltie rebuild costs: the pull request CI (the same workflows), wall clock.
    if True:
        d = [(max(t(s["updatedAt"]) for s in ss) - min(t(s["createdAt"]) for s in ss)).total_seconds()
             for p in prs if (ss := [s for s in suites(p) if s["workflowRun"]["workflow"]["name"] in tested_names])]
        ci_seconds = median(d)

    # When each pull request's CI last ran, i.e. which main it was tested against.
    fps = {}
    def fp(sha):
        if sha not in fps:
            f = footprint(repo, sha)
            fps[sha] = None if f is None else (set(f["footprint"]["writes"]), set(f["footprint"]["reads"]), f["footprint"]["leaky"])
        return fps[sha]

    info = {}
    for l in landings:
        n = pr_of.get(l["sha"])
        row = {"pr": n, "direct": n is None}
        if n is not None:
            ss = [s for s in suites(prs_by[n]) if s["workflowRun"]["workflow"]["name"] in tested_names] if n in prs_by else []
            if ss:
                tested = max(t(s["createdAt"]) for s in ss)
                between = [m for m in landings[:idx[l["sha"]]] if t(m["at"]) > tested]
                row.update(tested=iso(tested), between=len(between))
                me = fp(l["sha"])
                if me is None:
                    row["overlap"] = None
                else:
                    landed = set()
                    unknown = False
                    for m in between:
                        o = fp(m["sha"])
                        if o is None:
                            unknown = True
                        else:
                            landed |= o[0]
                    hit = sorted(landed & me[1])
                    row["overlap"] = bool(hit) or (unknown and bool(between))
                    row["overlap_targets"] = hit[:5]
                    row["landed_write"] = bool(landed & me[0])
                    row["leaky"] = me[2]
            else:
                row["untested"] = True
        info[l["sha"]] = row

    # Red episodes per workflow, merged across workflows by their first red landing.
    episodes = {}
    for name in ci_names:
        seq = [(i, latest[(l["sha"], name)]["conclusion"]) for i, l in enumerate(landings) if (l["sha"], name) in latest]
        seq = [(i, "g" if c in GOOD else "r" if c in BAD else None) for i, c in seq]
        seq = [(i, c) for i, c in seq if c]
        last_green = None
        k = 0
        while k < len(seq):
            i, c = seq[k]
            if c == "g":
                last_green = i
                k += 1
                continue
            if last_green is None:
                k += 1
                continue
            j = k
            while j < len(seq) and seq[j][1] == "r":
                j += 1
            reds = j - k
            green_at = seq[j][0] if j < len(seq) else None
            key = landings[i]["sha"]
            e = episodes.setdefault(key, {"first_red": key, "first_red_index": i, "range": [last_green + 1, i],
                                          "workflows": [], "persistent": False, "red_seconds": 0})
            e["workflows"].append(name)
            e["persistent"] |= reds >= 2
            if green_at is not None:
                e["red_seconds"] = max(e["red_seconds"], (t(landings[green_at]["at"]) - t(landings[i]["at"])).total_seconds())
            last_green = None
            k = j

    for e in episodes.values():
        cands = [landings[x]["sha"] for x in range(e["range"][0], e["range"][1] + 1)]
        rows = [info[s] for s in cands]
        e["candidates"] = len(cands)
        if any(r["direct"] or r.get("untested") for r in rows):
            e["verdict"] = "direct push or untested change in range"
        elif any(r.get("overlap") for r in rows):
            e["verdict"] = "stale base, overlapping landing: Sheltie rebuilds before landing"
        elif any(r.get("between") for r in rows):
            e["verdict"] = "stale base, no overlap: Sheltie lands it too"
        else:
            e["verdict"] = "tested on the current main: not an interaction"
        e["prs"] = [r["pr"] for r in rows]

    pr_rows = [r for r in info.values() if not r["direct"]]
    tested = [r for r in pr_rows if "tested" in r]
    stale = [r for r in tested if r.get("between")]
    reverify = [r for r in tested if r.get("overlap")]
    persistent = [e for e in episodes.values() if e["persistent"]]
    verdicts = {}
    for e in persistent:
        verdicts[e["verdict"]] = verdicts.get(e["verdict"], 0) + 1
    missing = sum(1 for l in landings if fp(l["sha"]) is None)
    # How many persistent episodes would show an overlapping landing by chance: each candidate
    # overlaps with the base rate seen across all tested pull requests.
    base = len(reverify) / len(tested) if tested else 0
    by_chance = sum(1 - (1 - base) ** e["candidates"] for e in persistent)
    res = {
        "repo": repo, "tool": conf["tool"], "landings": len(landings), "pr_landings": len(pr_rows),
        "direct_pushes": len(landings) - len(pr_rows), "tested": len(tested), "ci_workflows": ci_names, "post_merge_ci": bool(ci_names),
        "ci_seconds_median": ci_seconds, "flaky_reruns_on_main": flaky_reruns,
        "stale": len(stale), "median_between": median([r["between"] for r in tested]),
        "sheltie_rebuilds": len(reverify), "sheltie_merge_engine": sum(1 for r in tested if r.get("landed_write")),
        "sheltie_extra_ci_seconds": len(reverify) * (ci_seconds or 0),
        "leaky": sum(1 for r in tested if r.get("leaky")), "missing_footprints": missing,
        "red_episodes": len(episodes), "persistent_red": len(persistent),
        "persistent_red_seconds": sum(e["red_seconds"] for e in persistent),
        "verdicts": verdicts, "overlap_base_rate": base, "overlap_by_chance": by_chance, "episodes": sorted(episodes.values(), key=lambda e: e["first_red_index"]),
    }
    os.makedirs(os.path.join(HERE, "results"), exist_ok=True)
    json.dump(res, open(os.path.join(HERE, "results", f"breaks-{repo.replace('/', '__')}.json"), "w"), indent=1)
    print(f"{repo}: {len(landings)} landings ({len(pr_rows)} PRs, {len(tested)} with CI); CI {hm(ci_seconds)} {ci_names[:4]}; "
          f"stale {len(stale)} (median {res['median_between']} between); Sheltie rebuilds {len(reverify)} "
          f"(+{hm(res['sheltie_extra_ci_seconds'])} CI), leaky {res['leaky']}, missing fp {missing} | "
          f"red episodes {len(episodes)}, persistent {len(persistent)} ({hm(res['persistent_red_seconds'])} red): {verdicts}; overlap by chance {by_chance:.1f}")


if __name__ == "__main__":
    for r in sys.argv[1:]:
        main(r)
