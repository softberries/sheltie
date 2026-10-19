#!/bin/bash
# Exact footprints for every first-parent landing since 2026-07-21, per repository.
M=$(cd "$(dirname "$0")" && pwd); REPOS=${REPOS:-$M/repos}; BENCH=${BENCH:-$M/bench/target/release/bench}
python3 -c "import json;[print(r.replace('/','__'),c['tool']) for r,c in json.load(open('$M/repos.json')).items()]" | sort -k1 | while read d t; do
  git -C $REPOS/$d log --first-parent --since=2026-07-21T00:00:00Z --format='%H %P' | python3 -c "import sys,json;print(json.dumps([{'sha':l.split()[0],'parent':l.split()[1]} for l in sys.stdin if len(l.split())>1]))" > $M/pairs-$d.json
  echo "== $d $t $(date +%T)"
  $BENCH footprints $t $REPOS/$d $M/pairs-$d.json $M/fp/$d ${THREADS:-8} 2>&1 | tail -1
done
echo ALL DONE $(date +%T)
