#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../../.."
compose=(-f scripts/perf/temporal-postgres/compose.yaml)
if [[ "${PERF_TEMPORAL_TRACE:-0}" == 1 ]]; then
  compose+=(-f scripts/perf/temporal-postgres/handoffs/trace.compose.yaml)
fi
docker compose "${compose[@]}" up -d
for attempt in {1..90}; do
  if docker exec lightspeed-perf-temporal temporal --address temporal:7233 operator namespace describe --namespace default >/dev/null 2>&1; then
    for db in temporal temporal_visibility; do
      docker exec lightspeed-perf-temporal-postgres psql -v ON_ERROR_STOP=1 -U temporal -d "$db" -c 'CREATE EXTENSION IF NOT EXISTS pg_stat_statements;'
    done
    exit 0
  fi
  sleep 2
done
echo 'Temporal PostgreSQL deployment did not become ready' >&2
exit 1
