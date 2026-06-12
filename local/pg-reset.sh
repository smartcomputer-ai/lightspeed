#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/common.sh"

compose exec -T postgres dropdb \
  --if-exists \
  --force \
  -U "${POSTGRES_USER}" \
  "${POSTGRES_DB}"

compose exec -T postgres createdb \
  -U "${POSTGRES_USER}" \
  "${POSTGRES_DB}"

echo "Postgres database reset: ${POSTGRES_DB}"
