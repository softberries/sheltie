"""Applies the manual classification of escape candidates (read from each pull request's
failed jobs, follow-up commits and changed files) to results/escapes.json. A candidate with
a changed diff and no label here is reported as unclassified, so new data is not silently
labelled.

usage: python3 classify.py
"""
import json, os
from common import HERE

GLOBAL = "repository-wide check (distribution size, API list, version list, performance baseline)"
SUBSET = "test or code fix; PR CI is a subset of queue CI, failing test not identified"
QUEUE_ONLY = "failing jobs run only in the queue (wasm, iOS/Android, examples, unused deps)"
UNKNOWN = "code change; failing job and cause not determined"
LABELS = {
    **{("gradle/gradle", n): GLOBAL for n in (39148, 38806, 38890, 38586, 38757, 39309, 39075, 38760, 39206, 38645)},
    **{("gradle/gradle", n): SUBSET for n in (39491, 39422, 38404, 39234, 39321, 39297, 38762, 38563, 38086, 37320,
                                               38484, 37893, 36110, 38569)},
    **{("bevyengine/bevy", n): QUEUE_ONLY for n in (26048, 25361, 25529, 25331)},
    ("bevyengine/bevy", 24969): 'semantic conflict with main ("Fix example merge conflict")',
    ("bevyengine/bevy", 25883): "test fix after a PR-CI job failed in the queue; cause not determined",
    ("bevyengine/bevy", 25456): "code change after a PR-CI job failed in the queue; cause not determined",
    ("trinodb/trino", 30846): "change to CI workflow, only exercised in the queue",
    ("trinodb/trino", 30830): "change to CI workflow, only exercised in the queue",
    **{("trinodb/trino", n): UNKNOWN for n in (29974, 31332, 31148, 30871, 31153)},
    **{("rust-lang/rust-analyzer", n): UNKNOWN for n in (23467, 23382)},
}

path = os.path.join(HERE, "results", "escapes.json")
E = json.load(open(path))
for e in E:
    if e["diff_changed"] is True:
        e["class"] = LABELS.get((e["repo"], e["pr"]), "UNCLASSIFIED: read this pull request")
    elif e["diff_changed"] is False:
        e["class"] = "diff unchanged: rebase or merge of main only"
    else:
        e["class"] = "not determined (diff unavailable)"
json.dump(E, open(path, "w"), indent=1)
print(sum(e["class"].startswith("UNCLASSIFIED") for e in E), "unclassified of", len(E))
