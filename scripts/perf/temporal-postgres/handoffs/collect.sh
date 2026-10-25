#!/usr/bin/env bash
# Synthetic local diagnostic only. A successful baseline release build is required.
set -euo pipefail
cd "$(dirname "$0")/../../../.."
out=${1:?fresh output directory}
python=${2:?Python with pinned grpcio and opentelemetry-proto}
shift 2
[[ ! -e "$out" && ! -e "$out.spans.jsonl" ]] || { echo 'Use fresh output paths' >&2; exit 1; }
mkdir -p "$(dirname "$out")"
"$python" scripts/perf/temporal-postgres/handoffs/collector.py "$out.spans.jsonl" > "$out.collector.log" 2>&1 &
collector=$!
cleanup() { kill "$collector" 2>/dev/null || true; wait "$collector" || true; }
trap cleanup EXIT
for attempt in {1..30}; do
  if rg -q 'collector ready' "$out.collector.log"; then break; fi
  kill -0 "$collector"
  sleep 1
done
rg -q 'collector ready' "$out.collector.log"
status=0
PERF_TEMPORAL_TRACE=1 PERF_TASK_QUEUE=${PERF_TASK_QUEUE:-perf-handoffs} \
LIGHTSPEED_LOG_FORMAT=json \
RUST_LOG=warn,temporal_runtime=info,temporalio_sdk_core::worker::workflow=debug \
 scripts/perf/temporal-postgres/experiment.sh "$out" postgres12_pgx 8192 "$@" || status=$?
# The runtime has stopped; allow final cancelled-poll spans to flush.
sleep 3
exit "$status"
