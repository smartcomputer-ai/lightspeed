#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Keep the reset sequence explicit: recreate both databases, ledger the runtime
# schema through its owner, then clear only Lightspeed's object-store prefix.
"${SCRIPT_DIR}/pg-reset.sh"
"${SCRIPT_DIR}/pg-migrate.sh"
"${SCRIPT_DIR}/minio-reset.sh"

echo "Lightspeed local infra reset complete"
