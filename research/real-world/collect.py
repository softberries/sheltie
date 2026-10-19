"""Collect merged pull requests, Actions runs and first-parent landings for one repository.

usage: python3 -I collect.py owner/name   -> data/<owner__name>/{prs,runs,landings}.json
"""
import json, os, subprocess, sys, datetime as dt

HERE = os.path.dirname(os.path.abspath(__file__))
REPOS = os.environ.get("REPOS", os.path.join(HERE, "repos"))
CONF = json.load(open(os.path.join(HERE, "repos.json")))
NOW = dt.datetime(2026, 10, 19, tzinfo=dt.timezone.utc)
SINCE = NOW - dt.timedelta(days=90)
ISO = "%Y-%m-%dT%H:%M:%SZ"


def gh(*args):
    for attempt in range(5):
        r = subprocess.run(["gh", "api", *args], capture_output=True, text=True)
        if r.returncode == 0:
            return json.loads(r.stdout) if r.stdout.strip() else None
        if "rate limit" in r.stderr.lower() or any(c in r.stderr for c in ("502", "503", "504")) or "timeout" in r.stderr.lower() or "something went wrong" in r.stderr.lower():
            subprocess.run(["sleep", str(30 * (attempt + 1))])
            continue
        raise RuntimeError(f"{args[:2]}: {r.stderr.strip()[:400]}")
    raise RuntimeError(f"{args[:2]}: retries exhausted")


def gql(query, **v):
    args = ["graphql", "-f", f"query={query}"]
    for k, x in v.items():
        args += ["-F" if isinstance(x, int) else "-f", f"{k}={x}"]
    d = gh(*args)
    return d["data"]


PR_FIELDS = """number title mergedAt updatedAt createdAt mergedBy{login} author{login} mergeCommit{oid} headRefOid
  commits(last:1){nodes{commit{oid
    checkSuites(first:40){nodes{createdAt updatedAt conclusion workflowRun{event workflow{name}}}}
    %STATUS% }}}
  timelineItems(itemTypes:[ADDED_TO_MERGE_QUEUE_EVENT,REMOVED_FROM_MERGE_QUEUE_EVENT,LABELED_EVENT,UNLABELED_EVENT],last:80){nodes{__typename
    ... on AddedToMergeQueueEvent{createdAt} ... on RemovedFromMergeQueueEvent{createdAt reason}
    ... on LabeledEvent{createdAt label{name}} ... on UnlabeledEvent{createdAt label{name}}}}
  %COMMENTS%"""


def fields(conf):
    f = PR_FIELDS.replace("%STATUS%", "status{contexts{context state createdAt}}" if conf["kind"] == "tide" else "")
    return f.replace("%COMMENTS%", "comments(last:40){nodes{author{login} createdAt bodyText}}" if conf["kind"] == "bors" else "")


def prs_by_update(repo, conf):
    owner, name = repo.split("/")
    q = "query($o:String!,$n:String!,$b:String!,$c:String){repository(owner:$o,name:$n){pullRequests(states:MERGED,baseRefName:$b,orderBy:{field:UPDATED_AT,direction:DESC},first:%d,after:$c){pageInfo{hasNextPage endCursor} nodes{%s}}}}"
    page = 25 if conf["kind"] != "tide" else 10
    out, cursor = [], None
    while True:
        v = dict(o=owner, n=name, b=conf["branch"])
        if cursor:
            v["c"] = cursor
        d = gql(q % (page, fields(conf)), **v)["repository"]["pullRequests"]
        nodes = d["nodes"]
        out += [p for p in nodes if p["mergedAt"] and p["mergedAt"] >= SINCE.strftime(ISO)]
        if not d["pageInfo"]["hasNextPage"] or (nodes and nodes[-1]["updatedAt"] < SINCE.strftime(ISO)):
            break
        cursor = d["pageInfo"]["endCursor"]
        print(f"  {repo}: {len(out)} PRs", file=sys.stderr, flush=True)
    return out


def prs_by_number(repo, conf, numbers):
    owner, name = repo.split("/")
    out = []
    for i in range(0, len(numbers), 5):
        chunk = numbers[i:i + 5]
        body = " ".join(f"p{n}: pullRequest(number:{n}){{{fields(conf)}}}" for n in chunk)
        d = gql("query($o:String!,$n:String!){repository(owner:$o,name:$n){%s}}" % body, o=owner, n=name)["repository"]
        out += [v for v in d.values() if v]
        print(f"  {repo}: {len(out)}/{len(numbers)} PRs", file=sys.stderr, flush=True)
    return out


def runs(repo, extra, a, b):
    """Actions runs created in [a, b), halving the window while a query would exceed 1000."""
    q = f"repos/{repo}/actions/runs?per_page=100&created={a.strftime(ISO)}..{(b - dt.timedelta(seconds=1)).strftime(ISO)}{extra}"
    total = gh(q.replace("per_page=100", "per_page=1"))["total_count"]
    if total >= 1000 and b - a > dt.timedelta(hours=2):
        mid = a + (b - a) / 2
        return runs(repo, extra, a, mid) + runs(repo, extra, mid, b)
    pages = gh(q, "--paginate", "--slurp")
    keep = ("id", "name", "workflow_id", "event", "head_sha", "head_branch", "conclusion", "status", "run_attempt",
            "created_at", "run_started_at", "updated_at")
    return [{k: r.get(k) for k in keep} for p in pages for r in p["workflow_runs"]]


def landings(repo, conf):
    d = os.path.join(REPOS, repo.replace("/", "__"))
    out = subprocess.check_output(["git", "-C", d, "log", "--first-parent", f"--since={SINCE.strftime(ISO)}",
                                   "--format=%H%x1f%P%x1f%ct%x1f%s", "HEAD"], text=True)
    res = []
    for line in out.splitlines():
        sha, parents, ct, subj = line.split("\x1f", 3)
        ps = parents.split()
        if not ps:
            continue
        res.append({"sha": sha, "parent": ps[0], "merge": len(ps) > 1,
                    "at": dt.datetime.fromtimestamp(int(ct), dt.timezone.utc).strftime(ISO), "subject": subj})
    res.reverse()
    return res


def main(repo):
    conf = CONF[repo]
    outdir = os.path.join(HERE, "data", repo.replace("/", "__"))
    os.makedirs(outdir, exist_ok=True)
    path = lambda f: os.path.join(outdir, f)
    if not os.path.exists(path("landings.json")):
        json.dump(landings(repo, conf), open(path("landings.json"), "w"))
    if not os.path.exists(path("prs.json")):
        if conf["kind"] == "bors":
            import re
            ls = json.load(open(path("landings.json")))
            nums = sorted({int(m.group(1)) for l in ls if (m := re.match(r"Auto merge of #(\d+)", l["subject"]))})
            prs = prs_by_number(repo, conf, nums)
        else:
            prs = prs_by_update(repo, conf)
        json.dump(prs, open(path("prs.json"), "w"))
    if not os.path.exists(path("runs.json")):
        r = []
        if conf.get("actions", True):
            day = dt.timedelta(days=1)
            a = SINCE
            while a < NOW:
                b = min(a + 7 * day, NOW)
                r += runs(repo, f"&branch={conf['branch']}&event=push", a, b)
                if conf["kind"] == "mq":
                    r += runs(repo, "&event=merge_group", a, b)
                a = b
        json.dump(r, open(path("runs.json"), "w"))
    print(repo, "collected", flush=True)


if __name__ == "__main__":
    for repo in sys.argv[1:]:
        try:
            main(repo)
        except Exception as e:
            print(repo, "ERR", e, flush=True)
