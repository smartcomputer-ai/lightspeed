#!/usr/bin/env bash
# Run a separate, explicitly profiled repeat after the unprofiled baseline.
set -euo pipefail
config=${1:?scenario configuration}
out=${2:?new output directory}
if [[ -e "$out" ]]; then
  echo 'Use a new output directory to preserve prior data' >&2
  exit 1
fi
mkdir -p "$out"
npx tsx scripts/perf/run.ts "$config" "$out" > "$out/driver.log" 2>&1 &
driver=$!
for attempt in {1..1800}; do
  if [[ -f "$out/pg-before.csv" ]]; then break; fi
  if ! kill -0 "$driver" 2>/dev/null; then wait "$driver"; exit 1; fi
  sleep 1
done
if [[ ! -f "$out/pg-before.csv" ]]; then
  echo 'Fixture preparation did not finish within 30 minutes' >&2
  wait "$driver"
  exit 1
fi
date -u +%FT%TZ > "$out/profile-start.txt"
docker exec lightspeed-perf-temporal wget -T 40 -qO- 'http://127.0.0.1:7936/debug/pprof/profile?seconds=30' > "$out/temporal-cpu.pb.gz" &
cpu=$!
docker exec lightspeed-perf-temporal wget -T 40 -qO- 'http://127.0.0.1:7936/debug/pprof/allocs?seconds=30' > "$out/temporal-allocs.pb.gz" &
allocs=$!
status=0
wait "$driver" || status=$?
wait "$cpu" || status=$?
wait "$allocs" || status=$?
date -u +%FT%TZ > "$out/profile-end.txt"
printf '%s\n' "$status" > "$out/profile.exit"
exit "$status"
