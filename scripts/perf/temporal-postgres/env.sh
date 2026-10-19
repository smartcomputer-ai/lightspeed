# Source from the repository root after scripts/dev/env.sh. Local synthetic only.
export TEMPORAL_ADDRESS=http://127.0.0.1:17233
export LIGHTSPEED_POSTGRES_URL=postgres://lightspeed:lightspeed@localhost:15432/lightspeed_perf_postgres
export LIGHTSPEED_AUTH_MODE=single LIGHTSPEED_ROLES=gateway,sessions
export LIGHTSPEED_TASK_QUEUE=perf-postgres LIGHTSPEED_OBJECT_STORE_PREFIX=perf-postgres
export LIGHTSPEED_TEMPORAL_METRICS_BIND=127.0.0.1:19092
export PERF_PG_DATABASE=lightspeed_perf_postgres PERF_TEMPORAL_POSTGRES=1
