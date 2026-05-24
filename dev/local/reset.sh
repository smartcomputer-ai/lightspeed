#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

"${SCRIPT_DIR}/pg-reset.sh"
"${SCRIPT_DIR}/pg-migrate.sh"
"${SCRIPT_DIR}/minio-reset.sh"

echo "Forge local infra reset complete"
