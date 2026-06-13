#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/common.sh"

compose run --rm --no-deps \
  -e MC_HOST_local="http://${MINIO_ROOT_USER}:${MINIO_ROOT_PASSWORD}@minio:9000" \
  mc mb --ignore-existing "local/${FORGE_OBJECT_STORE_BUCKET}"

echo "bucket ensured: ${FORGE_OBJECT_STORE_BUCKET}"
