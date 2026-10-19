import json, os, re, statistics, subprocess, datetime as dt

HERE = os.path.dirname(os.path.abspath(__file__))
BENCH = os.environ.get("BENCH", os.path.join(HERE, "bench", "target", "release", "bench"))
CONF = json.load(open(os.path.join(HERE, "repos.json")))
SINCE = "2026-07-21T00:00:00Z"


def t(s):
    return dt.datetime.strptime(s, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=dt.timezone.utc)


def iso(d):
    return d.strftime("%Y-%m-%dT%H:%M:%SZ")


def load(repo):
    d = os.path.join(HERE, "data", repo.replace("/", "__"))
    j = lambda f: json.load(open(os.path.join(d, f)))
    landings = [l for l in j("landings.json") if l["at"] >= SINCE]
    prs = j("prs.json")
    if CONF[repo].get("rebase"):
        landings = fold(landings, prs)
    return landings, prs, j("runs.json")


def fold(landings, prs):
    """Rebase-merged pull requests land several commits at once: one change per pull request,
    from the parent of its first commit to its last (commits pushed within a minute of it)."""
    got = {l["sha"] for l in pr_landing_map(landings, prs).values()}
    out, k = [], 0
    while k < len(landings):
        j = k
        while j < len(landings) and landings[j]["sha"] not in got:
            j += 1
        if j == len(landings):
            out += landings[k:]
            break
        first = j
        while first > k and abs((t(landings[first - 1]["at"]) - t(landings[j]["at"])).total_seconds()) <= 60:
            first -= 1
        out += landings[k:first]
        out.append(dict(landings[j], parent=landings[first]["parent"], folded=j - first + 1))
        k = j + 1
    return out


def cache(repo):
    return os.path.join(HERE, "fp-folded" if CONF[repo].get("rebase") else "fp", repo.replace("/", "__"))


def footprint(repo, sha):
    try:
        v = json.load(open(os.path.join(cache(repo), sha + ".json")))
    except FileNotFoundError:
        return None
    return None if "error" in v else v


def relate(repo, pairs):
    """Relation of each (a, b) pair of commits, from cached footprints."""
    if not pairs:
        return {}
    path = os.path.join(HERE, "tmp-pairs.json")
    json.dump([{"a": a, "b": b} for a, b in pairs], open(path, "w"))
    out = json.loads(subprocess.check_output([BENCH, "relate", cache(repo), path]))
    return {(r["a"], r["b"]): r["relation"] for r in out}


def queue_replay(repo, ci_seconds, changes):
    path = os.path.join(HERE, "tmp-queue.json")
    json.dump({"ci_seconds": int(ci_seconds), "changes": changes}, open(path, "w"))
    return json.loads(subprocess.check_output([BENCH, "queue", cache(repo), path]))


def pct(xs, p):
    xs = sorted(xs)
    if not xs:
        return None
    k = (len(xs) - 1) * p / 100
    f = int(k)
    return xs[f] if f + 1 >= len(xs) else xs[f] + (xs[f + 1] - xs[f]) * (k - f)


def median(xs):
    return statistics.median(xs) if xs else None


def pr_landing_map(landings, prs):
    """PR number -> landing, by merge commit, then by '(#N)' / 'Merge pull request #N' in the subject."""
    by_sha = {l["sha"]: l for l in landings}
    out = {}
    for p in prs:
        mc = (p.get("mergeCommit") or {}).get("oid")
        if mc in by_sha:
            out[p["number"]] = by_sha[mc]
    for l in landings:
        m = re.search(r"\(#(\d+)\)\s*$", l["subject"]) or re.match(r"Merge pull request #(\d+)", l["subject"]) \
            or re.match(r"Auto merge of #(\d+)", l["subject"])
        if m and int(m.group(1)) not in out:
            out[int(m.group(1))] = l
    return out


def suites(pr, events=("pull_request",)):
    nodes = ((pr.get("commits") or {}).get("nodes") or [])
    if not nodes:
        return []
    return [s for s in nodes[0]["commit"]["checkSuites"]["nodes"]
            if s.get("workflowRun") and s["workflowRun"]["event"] in events]


def hm(seconds):
    if seconds is None:
        return "–"
    s = int(round(seconds))
    if s < 60:
        return f"{s} s"
    m = s // 60
    if m < 60:
        return f"{m} min"
    h, m = divmod(m, 60)
    if h < 48:
        return f"{h} h {m} min" if m else f"{h} h"
    d, h = divmod(h, 24)
    return f"{d} d {h} h"
