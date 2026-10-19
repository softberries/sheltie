"""TeamCity statuses on gradle's merge-queue commits -> data/gradle__gradle/statuses.json."""
import json, os, subprocess
from concurrent.futures import ThreadPoolExecutor
HERE = os.path.dirname(os.path.abspath(__file__))
L = json.load(open(os.path.join(HERE, "data/gradle__gradle/landings.json")))
def get(sha):
    r = subprocess.run(["gh", "api", f"repos/gradle/gradle/commits/{sha}/statuses?per_page=100", "--paginate", "--slurp"], capture_output=True, text=True)
    if r.returncode:
        return sha, None
    return sha, [{"context": s["context"], "state": s["state"], "created_at": s["created_at"]} for p in json.loads(r.stdout) for s in p]
with ThreadPoolExecutor(6) as ex:
    out = dict(ex.map(get, [l["sha"] for l in L if l["at"] >= "2026-07-21"]))
json.dump(out, open(os.path.join(HERE, "data/gradle__gradle/statuses.json"), "w"))
print(len(out), sum(v is None for v in out.values()))
