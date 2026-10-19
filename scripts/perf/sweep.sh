#!/usr/bin/env bash
set -euo pipefail
failed=0
# Start release runtime/simulator first. All data is synthetic and local.
artifact=${1:?artifact directory required}
mkdir -p "$artifact"
for scenario in rapid active-readers idle-readers history; do
  for rate in 2 10 40; do
    name="$scenario-$rate"
    python3 - "$scenario" "$rate" "$artifact/$name.json" <<'PY'
import json,sys
x=json.load(open(f'scripts/perf/fixtures/{sys.argv[1]}.json'))
x.update(rate=int(sys.argv[2]),durationMs=30000)
json.dump(x,open(sys.argv[3],'w'),indent=2)
PY
    docker exec lightspeed-postgres psql -U lightspeed -d lightspeed -c 'SELECT pg_stat_statements_reset();' > "$artifact/$name-pg-reset.txt"
    npx tsx scripts/perf/run.ts "$artifact/$name.json" "$artifact/$name" > "$artifact/$name.log" 2>&1 || { echo "$name returned nonzero; inspect results" >&2; failed=1; }
    docker exec lightspeed-postgres psql -U lightspeed -d lightspeed --csv -c 'SELECT calls,total_exec_time,mean_exec_time,rows,shared_blks_hit,shared_blks_read,query FROM pg_stat_statements ORDER BY total_exec_time DESC LIMIT 40;' > "$artifact/$name-pg.csv"
  done
done

exit "$failed"
