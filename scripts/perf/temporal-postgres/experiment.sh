#!/usr/bin/env bash
# Controlled local A/B arm. No database/volume/statistics reset; new fixture IDs.
set -euo pipefail
cd "$(dirname "$0")/../../.."
out=${1:?new artifact directory}
driver=${2:?postgres12 or postgres12_pgx}
fds=${3:?runtime soft descriptor limit}
shift 3
case "$driver" in postgres12|postgres12_pgx) ;; *) echo 'Unsupported driver' >&2; exit 1;; esac
[[ "$fds" =~ ^[0-9]+$ ]] && (( fds >= 1024 && fds <= 65536 )) || exit 1
[[ ! -e "$out" ]] || { echo 'Use a fresh output directory' >&2; exit 1; }
mkdir -p "$out/sweep"
out=$(realpath "$out")
export PERF_TEMPORAL_SQL_DRIVER=$driver
scripts/perf/temporal-postgres/up.sh > "$out/setup.log" 2>&1
source scripts/dev/env.sh >/dev/null
source scripts/perf/temporal-postgres/env.sh
runtime_pid= simulator_pid= observer_pid= services_pid=
cleanup() {
  for pid in "$observer_pid" "$services_pid" "$runtime_pid" "$simulator_pid"; do
    [[ -z "$pid" ]] || kill "$pid" 2>/dev/null || true
  done
  wait || true
}
trap cleanup EXIT
(ulimit -Sn "$fds"; exec "${PERF_RUNTIME_BINARY:-target/release/lightspeed-runtime}") > "$out/runtime.log" 2>&1 &
runtime_pid=$!
target/release/llm-simulator scripts/perf/fixtures/simulator.json > "$out/simulator.log" 2>&1 &
simulator_pid=$!
ready=false
for attempt in {1..60}; do
  if curl -fsS http://127.0.0.1:19090/metrics >/dev/null 2>&1 && curl -fsS http://127.0.0.1:19092/metrics >/dev/null 2>&1 && curl -s --max-time 1 -o /dev/null http://127.0.0.1:18080/rpc; then ready=true; break; fi
  kill -0 "$runtime_pid" "$simulator_pid"
  sleep 1
done
[[ "$ready" == true ]] || { echo "Local runtime/simulator did not become ready" >&2; exit 1; }
kill -0 "$runtime_pid" "$simulator_pid"
cat "/proc/$runtime_pid/limits" > "$out/runtime-limits.txt"
printf '%s\n' "$runtime_pid" > "$out/runtime.pid"
printf '%s\n' "$simulator_pid" > "$out/simulator.pid"
sha256sum "${PERF_RUNTIME_BINARY:-target/release/lightspeed-runtime}" target/release/llm-simulator > "$out/binary-sha256.txt"
if [[ -n "${PERF_TEMPORAL_SERVER_BINARY:-}" ]]; then
  sha256sum "$PERF_TEMPORAL_SERVER_BINARY" > "$out/server-binary-sha256.txt"
fi
printf 'driver=%s\nruntime_soft_fds=%s\ntask_queue=%s\nserver_tracing=%s\nlog_format=%s\n' \
  "$driver" "$fds" "$LIGHTSPEED_TASK_QUEUE" "${PERF_TEMPORAL_TRACE:-0}" \
  "${LIGHTSPEED_LOG_FORMAT:-compact}" > "$out/arm.txt"
printf 'session_workflow_pollers=%s\n' "${LIGHTSPEED_SESSION_WORKFLOW_POLLERS:-SDK-default}" >> "$out/arm.txt"
docker exec lightspeed-perf-temporal cat /etc/temporal/config/docker.yaml | sed '/password:/d' > "$out/temporal-effective-redacted.yaml"
if [[ -n "${PERF_TEMPORAL_SHARD_IO_CONCURRENCY:-}" ]]; then
  docker exec lightspeed-perf-temporal cat /etc/temporal/config/perf-shard-io.yaml > "$out/temporal-dynamic.yaml"
  printf 'shard_io_concurrency=%s\n' "$PERF_TEMPORAL_SHARD_IO_CONCURRENCY" >> "$out/arm.txt"
fi
if [[ -n "${PERF_TEMPORAL_DURATION_PROFILE:-}" ]]; then
  docker exec lightspeed-perf-temporal cat /etc/temporal/config/perf-duration.yaml > "$out/temporal-dynamic.yaml"
  printf 'duration_profile=%s\n' "$PERF_TEMPORAL_DURATION_PROFILE" >> "$out/arm.txt"
fi
if [[ -n "${PERF_RESUME_FIXTURE:-}" ]]; then
  npx tsx scripts/perf/resume-fixture.ts "$PERF_RESUME_FIXTURE" "$out/continuity.json" > "$out/continuity.log" 2>&1
fi
python3 scripts/perf/observe.py "$out/observations.jsonl" "$runtime_pid" "$simulator_pid" > "$out/observer.log" 2>&1 &
observer_pid=$!
python3 scripts/perf/temporal-postgres/observe-services.py "$out" > "$out/services-observer.log" 2>&1 &
services_pid=$!
status=0
for step in "$@"; do
  python3 - "$step" "$out/sweep/$step.json" <<'PY'
import json,sys,re
m=re.fullmatch(r'(rapid|history|active-readers|idle-readers)-(\d+)(-profile)?',sys.argv[1])
if not m:raise ValueError('Unsupported scenario name')
x=json.load(open(f'scripts/perf/fixtures/{m[1]}.json'))
x.update(rate=int(m[2]),durationMs=30000)
if m[1]=='idle-readers':x['concurrency']=4096
json.dump(x,open(sys.argv[2],'w'),indent=2)
PY
  code=0
  if [[ "$step" == *-profile ]]; then
    scripts/perf/temporal-postgres/profile-step.sh "$out/sweep/$step.json" "$out/sweep/$step" > "$out/sweep/$step.log" 2>&1 || code=$?
  else
    npx tsx scripts/perf/run.ts "$out/sweep/$step.json" "$out/sweep/$step" > "$out/sweep/$step.log" 2>&1 || code=$?
  fi
  printf '%s\n' "$code" > "$out/sweep/$step.exit"
  (( code == 0 )) || status=$code
done
docker logs lightspeed-perf-temporal > "$out/temporal-server.log" 2>&1
printf '%s\n' "$status" > "$out/arm.exit"
exit "$status"
