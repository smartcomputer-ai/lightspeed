#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/common.sh"

"${SCRIPT_DIR}/minio-ensure.sh"

TARGET="local/${FORGE_OBJECT_STORE_BUCKET}"
if [[ -n "${FORGE_OBJECT_STORE_PREFIX}" ]]; then
  TARGET="${TARGET}/${FORGE_OBJECT_STORE_PREFIX#/}"
fi

compose run --rm --no-deps \
  -e MC_HOST_local="http://${MINIO_ROOT_USER}:${MINIO_ROOT_PASSWORD}@minio:9000" \
  mc rm --recursive --force "${TARGET}"

echo "MinIO objects reset: ${TARGET}"
