"""Queue failures on pull requests Sheltie would have landed at once, that were changed
afterwards: which jobs failed in the queue and what the follow-up commits were.

usage: python3 escapes.py  -> results/escapes.json
"""
import json, os, subprocess
from common import *

REPOS = ["trinodb/trino", "bevyengine/bevy", "junit-team/junit-framework", "rust-lang/rust-analyzer", "gradle/gradle"]


def gh(path):
    r = subprocess.run(["gh", "api", path, "--paginate", "--slurp"], capture_output=True, text=True)
    return json.loads(r.stdout) if r.returncode == 0 else None


def cases(repo):
    q = json.load(open(os.path.join(HERE, "results", f"queue-{repo.replace('/', '__')}.json")))
    L, P, R = load(repo)
    dec = {l["sha"]: d for l, (w, d) in zip(L, q["sheltie"]["per_change"])}
    byn = {p["number"]: p for p in P}
    for row in q["rows"]:
        if dec[row["landing"]] != "land_now":
            continue
        p = byn[row["pr"]]
        ej = sorted(t(n["createdAt"]) for n in p["timelineItems"]["nodes"]
                    if n["__typename"] == "RemovedFromMergeQueueEvent" and n.get("reason") in ("failed_checks", "checks_timed_out"))
        if not ej:
            continue
        last = max((t(s["createdAt"]) for s in suites(p)), default=None)
        if last is None or last < ej[0]:
            continue  # re-queued unchanged
        yield repo, p, ej, R


out = []
for repo in REPOS:
    for repo, p, ej, R in cases(repo):
        n = p["number"]
        failed_runs = [r for r in R if r["event"] == "merge_group" and f"/pr-{n}-" in (r["head_branch"] or "")
                       and r["conclusion"] in ("failure", "timed_out") and t(r["created_at"]) <= ej[-1]]
        jobs = []
        for r in failed_runs:
            j = gh(f"repos/{repo}/actions/runs/{r['id']}/jobs?per_page=100&filter=latest")
            if j:
                jobs += [f"{r['name']} / {x['name']}" for pg in j for x in pg["jobs"] if x["conclusion"] in ("failure", "timed_out")]
        commits = gh(f"repos/{repo}/pulls/{n}/commits?per_page=100") or []
        after = [{"date": c["commit"]["committer"]["date"], "msg": c["commit"]["message"].splitlines()[0][:120]}
                 for pg in commits for c in pg if t(c["commit"]["committer"]["date"]) > ej[0]]
        out.append({"repo": repo, "pr": n, "title": p["title"][:120], "ejections": [iso(x) for x in ej],
                    "failed_jobs": sorted(set(jobs)), "commits_after": after})
        print(repo, n, len(jobs), len(after), flush=True)
json.dump(out, open(os.path.join(HERE, "results", "escapes.json"), "w"), indent=1)
print(len(out))
