#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/common.sh"

# Migrations are idempotent (CREATE ... IF NOT EXISTS) and must apply in
# numeric order, matching PgStore::migrate.
for MIGRATION in "${REPO_ROOT}/crates/store-pg/migrations/"*.sql; do
  compose exec -T postgres psql \
    -U "${POSTGRES_USER}" \
    -d "${POSTGRES_DB}" \
    -v ON_ERROR_STOP=1 \
    < "${MIGRATION}"
  echo "Postgres schema applied: ${MIGRATION}"
done
