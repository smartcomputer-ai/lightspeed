#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/common.sh"

compose up -d postgres pgadmin minio temporal

echo "Waiting for Postgres..."
until compose exec -T postgres pg_isready -U "${POSTGRES_USER}" -d "${POSTGRES_DB}" >/dev/null 2>&1; do
  sleep 1
done

echo "Waiting for MinIO..."
until "${SCRIPT_DIR}/minio-ensure.sh" >/dev/null 2>&1; do
  sleep 1
done

echo "Checking pgAdmin..."
for _ in {1..30}; do
  if [[ "$(docker inspect -f '{{.State.Running}} {{.State.Restarting}}' "${PGADMIN_CONTAINER_NAME:-lightspeed-pgadmin}" 2>/dev/null)" == "true false" ]]; then
    break
  fi
  sleep 1
done

if [[ "$(docker inspect -f '{{.State.Running}} {{.State.Restarting}}' "${PGADMIN_CONTAINER_NAME:-lightspeed-pgadmin}" 2>/dev/null)" != "true false" ]]; then
  echo "pgAdmin did not start cleanly. Recent logs:" >&2
  compose logs --tail=80 pgadmin >&2
  exit 1
fi

echo "Checking Temporal..."
for _ in {1..60}; do
  if nc -z localhost "${TEMPORAL_PORT}" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

if ! nc -z localhost "${TEMPORAL_PORT}" >/dev/null 2>&1; then
  echo "Temporal did not open port ${TEMPORAL_PORT}. Recent logs:" >&2
  compose logs --tail=80 temporal >&2
  exit 1
fi

cat <<EOF

Lightspeed local infra is up.

Postgres:
  url:     ${LIGHTSPEED_TEST_POSTGRES_URL}
  pgAdmin: http://localhost:${PGADMIN_PORT}

Blobstore:
  bucket:        ${LIGHTSPEED_OBJECT_STORE_BUCKET}
  S3 endpoint:   ${LIGHTSPEED_OBJECT_STORE_ENDPOINT}
  MinIO console: http://localhost:${MINIO_CONSOLE_PORT}

Temporal:
  target: http://localhost:${TEMPORAL_PORT}
  UI:     http://localhost:${TEMPORAL_UI_PORT}

Suggested env:
  source local/env.sh
EOF
