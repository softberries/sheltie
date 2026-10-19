"""How much the "tested on the branch as of landing" assumption changes Sheltie's replay:
the same queue entries with each change's real base (when its pull request CI last started)
and with none. Run after queues.py.

usage: python3 roi_assumption.py  -> results/roi-assumption.json
"""
import json, os
from common import *

out = {}
for repo in ["rust-lang/rust-analyzer", "junit-team/junit-framework", "trinodb/trino", "bevyengine/bevy",
             "gradle/gradle", "rust-lang/rust"]:
    q = json.load(open(os.path.join(HERE, "results", f"queue-{repo.replace('/', '__')}.json")))
    changes = q["replay_input"]
    a = queue_replay(repo, q["ci"]["median_seconds"], changes)["replay"]["sheltie"]
    b = queue_replay(repo, q["ci"]["median_seconds"], [{k: v for k, v in c.items() if k != "based_at"} for c in changes])["replay"]["sheltie"]
    out[repo] = {"with_base": a, "without_base": b}
    print(f"{repo}: with base {a['ci_runs']} CI runs, total wait {hm(a['total_wait_seconds'])}; "
          f"without {b['ci_runs']} CI runs, total wait {hm(b['total_wait_seconds'])}")
json.dump(out, open(os.path.join(HERE, "results", "roi-assumption.json"), "w"), indent=1)
