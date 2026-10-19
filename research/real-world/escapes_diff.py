"""For each escape candidate: did the pull request's own diff change after the queue failure?
Compares the PR diff (three-dot, against its merge base) at the head it had when ejected with
its final head. Same added/removed lines per file = only a rebase or a merge of main.

usage: python3 escapes_diff.py -> results/escapes.json (adds "before_head", "diff_changed", "changed_files")
"""
import json, os, subprocess
from common import *

Q = """query($o:String!,$n:String!,$num:Int!){repository(owner:$o,name:$n){pullRequest(number:$num){
  headRefOid baseRefName
  timelineItems(itemTypes:[HEAD_REF_FORCE_PUSHED_EVENT,PULL_REQUEST_COMMIT],last:250){nodes{__typename
    ... on HeadRefForcePushedEvent{createdAt beforeCommit{oid} afterCommit{oid}}
    ... on PullRequestCommit{commit{oid pushedDate committedDate}}}}}}}"""


def gh(*args):
    r = subprocess.run(["gh", "api", *args], capture_output=True, text=True)
    return json.loads(r.stdout) if r.returncode == 0 and r.stdout.strip() else None


def patch(repo, base, head):
    """file -> sorted added/removed lines of base...head, or None if unavailable."""
    c = gh(f"repos/{repo}/compare/{base}...{head}")
    if not c or not c.get("files") or len(c["files"]) >= 300:
        return None
    out = {}
    for f in c["files"]:
        lines = [l for l in (f.get("patch") or "").splitlines() if l[:1] in "+-" and not l.startswith(("+++", "---"))]
        out[f["filename"]] = sorted(lines) if f.get("patch") is not None else f["sha"]
    return out


E = json.load(open(os.path.join(HERE, "results", "escapes.json")))
# Compare against main just before the landing: after a merge commit the head is an ancestor
# of the branch, so comparing with the branch itself would be empty.
parent_of = {}
for repo in {e["repo"] for e in E}:
    q = json.load(open(os.path.join(HERE, "results", f"queue-{repo.replace('/', '__')}.json")))
    L = {l["sha"]: l for l in load(repo)[0]}
    parent_of.update({(repo, r["pr"]): L[r["landing"]]["parent"] for r in q["rows"]})
for e in E:
    o, n = e["repo"].split("/")
    d = gh("graphql", "-f", f"query={Q}", "-f", f"o={o}", "-f", f"n={n}", "-F", f"num={e['pr']}")
    pr = d["data"]["repository"]["pullRequest"]
    ej = t(e["ejections"][0])
    before = None
    # The head at the first failed ejection: the last commit pushed before it, unless a
    # force push after it names the head it replaced.
    for node in pr["timelineItems"]["nodes"]:
        if node["__typename"] == "HeadRefForcePushedEvent" and t(node["createdAt"]) > ej and node.get("beforeCommit"):
            before = node["beforeCommit"]["oid"]
            break
    if before is None:
        commits = [x["commit"] for x in pr["timelineItems"]["nodes"] if x["__typename"] == "PullRequestCommit"]
        prior = [c for c in commits if t(c.get("pushedDate") or c["committedDate"]) <= ej]
        before = prior[-1]["oid"] if prior else None
    e["before_head"], e["final_head"] = before, pr["headRefOid"]
    if before is None or before == pr["headRefOid"]:
        e["diff_changed"] = None if before is None else False
        e["changed_files"] = []
    else:
        a, b = patch(e["repo"], parent_of[(e["repo"], e["pr"])], before), patch(e["repo"], parent_of[(e["repo"], e["pr"])], pr["headRefOid"])
        if a is None or b is None:
            e["diff_changed"], e["changed_files"] = None, []
        else:
            ch = sorted(f for f in set(a) | set(b) if a.get(f) != b.get(f))
            e["diff_changed"], e["changed_files"] = bool(ch), ch[:20]
    print(e["repo"], e["pr"], e["diff_changed"], e["changed_files"][:4], flush=True)
json.dump(E, open(os.path.join(HERE, "results", "escapes.json"), "w"), indent=1)
