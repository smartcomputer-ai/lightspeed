#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../../.."
# Existing local dev PostgreSQL and MinIO must be running. Preserve all databases.
if ! docker exec lightspeed-postgres psql -U lightspeed -d postgres -tAc "SELECT 1 FROM pg_database WHERE datname='lightspeed_perf_postgres'" | grep -q 1; then
  docker exec lightspeed-postgres createdb -U lightspeed lightspeed_perf_postgres
fi
docker exec lightspeed-postgres psql -v ON_ERROR_STOP=1 -U lightspeed -d lightspeed_perf_postgres -c 'CREATE EXTENSION IF NOT EXISTS pg_stat_statements;'
source scripts/dev/env.sh >/dev/null
source scripts/perf/temporal-postgres/env.sh
target/release/lightspeed-runtime migrate
