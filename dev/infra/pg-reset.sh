#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/../lib/common.sh"

for DATABASE in "${POSTGRES_DB}" "${LIGHTSPEED_PLATFORM_POSTGRES_DB}"; do
  compose exec -T postgres dropdb \
    --if-exists \
    --force \
    -U "${POSTGRES_USER}" \
    "${DATABASE}"

  compose exec -T postgres createdb \
    -U "${POSTGRES_USER}" \
    "${DATABASE}"

  echo "Postgres database reset: ${DATABASE}"
done
