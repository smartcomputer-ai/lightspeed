#!/usr/bin/env bash
set -euo pipefail
failed=0
artifact=${1:?artifact directory}
for rate in 2 10 40 128 512 2048; do
  name="idle-readers-$rate"
  python3 - "$rate" "$artifact/$name.json" <<'PY'
import sys,json
x=json.load(open('scripts/perf/fixtures/idle-readers.json'))
x.update(rate=int(sys.argv[1]),concurrency=4096,durationMs=30000)
json.dump(x,open(sys.argv[2],'w'),indent=2)
PY
  npx tsx scripts/perf/run.ts "$artifact/$name.json" "$artifact/$name" > "$artifact/$name.log" 2>&1 || { echo "$name returned nonzero; inspect results" >&2; failed=1; }
done

exit "$failed"
