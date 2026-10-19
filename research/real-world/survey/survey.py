"""Classify how public repositories merge, from the public GitHub API (via `gh`).

usage: python3 -I survey.py owner/name [...]   -> out/<owner>__<name>.json
"""
import json, os, re, subprocess, sys, datetime as dt

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "out")
DAYS = 90
SINCE = (dt.datetime.now(dt.timezone.utc) - dt.timedelta(days=DAYS)).strftime("%Y-%m-%dT%H:%M:%SZ")
SAMPLE = 40


def gh(path, *args, ok404=False):
    r = subprocess.run(["gh", "api", path, *args], capture_output=True, text=True)
    if r.returncode != 0:
        if ok404 and ("404" in r.stderr or "Not Found" in r.stderr):
            return None
        raise RuntimeError(f"{path}: {r.stderr.strip()[:300]}")
    return json.loads(r.stdout) if r.stdout.strip() else None


def gql(query, **vars):
    args = ["graphql", "-f", f"query={query}"]
    for k, v in vars.items():
        args += ["-F", f"{k}={v}"]
    return gh(*args)["data"]


def runs_count(repo, branch=None, event=None, status=None):
    q = f"repos/{repo}/actions/runs?per_page=1&created=>={SINCE[:10]}"
    if branch: q += f"&branch={branch}"
    if event: q += f"&event={event}"
    if status: q += f"&status={status}"
    return gh(q)["total_count"]


def survey(repo):
    info = gh(f"repos/{repo}")
    branch = info["default_branch"]
    res = {"repo": repo, "branch": branch, "language": info.get("language"), "stars": info["stargazers_count"]}

    # Rulesets (public) and classic protection (only the enforcement level is public).
    rules = gh(f"repos/{repo}/rules/branches/{branch}") or []
    types = {r["type"]: r.get("parameters") for r in rules}
    res["ruleset_merge_queue"] = "merge_queue" in types
    rsc = types.get("required_status_checks") or {}
    res["ruleset_strict"] = rsc.get("strict_required_status_checks_policy")
    res["ruleset_required_checks"] = len(rsc.get("required_status_checks", []))
    pr_rule = types.get("pull_request") or {}
    res["allowed_merge_methods"] = pr_rule.get("allowed_merge_methods")
    prot = gh(f"repos/{repo}/branches/{branch}").get("protection") or {}
    res["classic_checks_enforcement"] = (prot.get("required_status_checks") or {}).get("enforcement_level")

    # Workflows that run in a merge queue; queue bots' config files.
    wf = gh(f"repos/{repo}/contents/.github/workflows", ok404=True) or []
    mg = []
    for f in wf:
        if f["name"].endswith((".yml", ".yaml")):
            raw = subprocess.run(["gh", "api", f"repos/{repo}/contents/{f['path']}", "-H", "Accept: application/vnd.github.raw"],
                                 capture_output=True, text=True).stdout
            if re.search(r"^\s*merge_group\s*:|[\[, ]merge_group[\],]|^\s*-\s*merge_group\s*$", raw, re.M):
                mg.append(f["name"])
    res["workflows"] = len(wf)
    res["merge_group_workflows"] = mg
    res["queue_bot_config"] = [p for p in [".mergify.yml", ".github/mergify.yml", ".mergify/config.yml", "bors.toml", ".github/bors.toml", ".prow.yaml", "OWNERS"]
                               if gh(f"repos/{repo}/contents/{p}", ok404=True) is not None]

    # Landings on the default branch in the window.
    commits = gh(f"repos/{repo}/commits?sha={branch}&since={SINCE}&per_page=100", "--paginate", "--slurp")
    commits = [c for page in commits for c in page]
    msgs = [c["commit"]["message"] for c in commits]
    res["commits_90d"] = len(commits)
    res["bors_merges"] = sum(1 for m in msgs if m.startswith("Auto merge of #") or m.startswith("Rollup merge of #"))
    res["reverts"] = sum(1 for m in msgs if m.startswith("Revert ") or m.lower().startswith("revert:") or m.startswith("revert "))
    res["fix_build"] = sum(1 for m in msgs if re.search(r"\b(fix|repair|unbreak)\w*\b.{0,20}\b(build|ci|main|master|tests?)\b", m.split("\n")[0], re.I)
                           and not m.startswith("Revert"))
    res["committers"] = {}
    for c in commits:
        k = (c.get("committer") or {}).get("login") or c["commit"]["committer"]["name"]
        res["committers"][k] = res["committers"].get(k, 0) + 1

    # Actions runs in the window.
    res["runs_push_main"] = runs_count(repo, branch=branch, event="push")
    res["runs_push_main_failed"] = runs_count(repo, branch=branch, event="push", status="failure")
    res["runs_merge_group"] = runs_count(repo, event="merge_group")
    res["runs_pull_request"] = runs_count(repo, event="pull_request")

    # Recent merged pull requests: who merged, queue events, up to date with the base at merge.
    owner, name = repo.split("/")
    d = gql("""query($o:String!,$n:String!){repository(owner:$o,name:$n){pullRequests(states:MERGED,first:%d,orderBy:{field:UPDATED_AT,direction:DESC}){
      totalCount nodes{number mergedAt createdAt headRefOid commits{totalCount} mergedBy{login}
      mergeCommit{oid parents(first:2){nodes{oid}}}
      timelineItems(itemTypes:[ADDED_TO_MERGE_QUEUE_EVENT,HEAD_REF_FORCE_PUSHED_EVENT,BASE_REF_FORCE_PUSHED_EVENT,MERGED_EVENT],first:50){nodes{__typename}}}}}}""" % SAMPLE,
            o=owner, n=name)
    prs = [p for p in d["repository"]["pullRequests"]["nodes"] if p["mergedAt"] and p["mergeCommit"]]
    prs.sort(key=lambda p: p["mergedAt"], reverse=True)
    sample = []
    for p in prs[:SAMPLE]:
        mc = p["mergeCommit"]
        parents = [x["oid"] for x in mc["parents"]["nodes"]]
        events = [x["__typename"] for x in p["timelineItems"]["nodes"]]
        row = {"n": p["number"], "merged": p["mergedAt"], "by": (p["mergedBy"] or {}).get("login"),
               "queued": "AddedToMergeQueueEvent" in events, "commits": p["commits"]["totalCount"]}
        if mc["oid"] == p["headRefOid"]:
            row["method"], row["behind"] = "fast-forward", 0
        elif len(parents) == 2:
            row["method"] = "merge"
            row["behind"] = gh(f"repos/{repo}/compare/{parents[0]}...{p['headRefOid']}")["behind_by"]
        else:
            base = parents[0] if parents else None
            # A multi-commit rebase merge: the parent is this PR's own previous commit.
            if base and p["commits"]["totalCount"] > 1:
                pulls = gh(f"repos/{repo}/commits/{base}/pulls", ok404=True) or []
                if any(x["number"] == p["number"] for x in pulls):
                    row["method"], row["behind"] = "rebase", None
                    sample.append(row)
                    continue
            row["method"] = "squash"
            cmp = gh(f"repos/{repo}/compare/{base}...{p['headRefOid']}", ok404=True) if base else None
            row["behind"] = cmp["behind_by"] if cmp else None
        sample.append(row)
    res["merged_prs_total"] = d["repository"]["pullRequests"]["totalCount"]
    res["sample"] = sample
    known = [r for r in sample if r["behind"] is not None]
    res["sample_up_to_date"] = f"{sum(1 for r in known if r['behind'] == 0)}/{len(known)}"
    res["sample_queued"] = sum(1 for r in sample if r["queued"])
    res["sample_merged_by"] = {}
    for r in sample:
        res["sample_merged_by"][r["by"]] = res["sample_merged_by"].get(r["by"], 0) + 1
    return res


if __name__ == "__main__":
    os.makedirs(OUT, exist_ok=True)
    for repo in sys.argv[1:]:
        path = os.path.join(OUT, repo.replace("/", "__") + ".json")
        if os.path.exists(path):
            continue
        try:
            r = survey(repo)
            json.dump(r, open(path, "w"), indent=1)
            print(repo, "ok", r["sample_up_to_date"], "queued", r["sample_queued"], "mq", r["ruleset_merge_queue"], r["merge_group_workflows"], flush=True)
        except Exception as e:
            print(repo, "ERR", e, flush=True)
