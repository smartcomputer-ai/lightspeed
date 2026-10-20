#!/usr/bin/env bash
# Ten bounded goroutine snapshots during an explicitly selected load step.
set -euo pipefail
step=${1:?step directory}
for attempt in {1..180}; do
  [[ ! -f "$step/pg-before.csv" ]] || break
  sleep 1
done
[[ -f "$step/pg-before.csv" ]] || { echo 'measurement did not start' >&2; exit 1; }
mkdir "$step/goroutines"
sleep 3
for i in {1..10}; do
  date -u +%FT%T.%NZ > "$step/goroutines/$i.start.txt"
  docker exec lightspeed-perf-temporal wget -T 5 -qO- \
    'http://127.0.0.1:7936/debug/pprof/goroutine?debug=2' > "$step/goroutines/$i.txt"
  date -u +%FT%T.%NZ > "$step/goroutines/$i.end.txt"
  sleep 2
done
