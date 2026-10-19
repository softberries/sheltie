"""Footprints for rebase-merged pull requests (repos.json "rebase": true), one per pull
request: from the parent of its first commit to its last. Run after footprints.sh; single
commits are linked from fp/, folded ones computed into fp-folded/.

usage: python3 footprints_folded.py
"""
import json, os, subprocess
from common import *

REPOS = os.environ.get("REPOS", os.path.join(HERE, "repos"))
for repo, conf in CONF.items():
    if not conf.get("rebase"):
        continue
    d = repo.replace("/", "__")
    out = cache(repo)
    os.makedirs(out, exist_ok=True)
    todo = []
    for l in load(repo)[0]:
        if l.get("folded", 1) > 1:
            todo.append({"sha": l["sha"], "parent": l["parent"]})
        elif not os.path.exists(os.path.join(out, l["sha"] + ".json")):
            os.link(os.path.join(HERE, "fp", d, l["sha"] + ".json"), os.path.join(out, l["sha"] + ".json"))
    pairs = os.path.join(HERE, f"pairs-folded-{d}.json")
    json.dump(todo, open(pairs, "w"))
    print(repo, len(todo), "folded pull requests", flush=True)
    subprocess.run([BENCH, "footprints", conf["tool"], os.path.join(REPOS, d), pairs, out, os.environ.get("THREADS", "8")], check=True)
