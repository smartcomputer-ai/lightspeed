#!/usr/bin/env bash
set -Eeuo pipefail

CODER_DEV_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export LIGHTSPEED_DEV_COMPOSE_FILE="${CODER_DEV_DIR}/compose.yaml"

# All normal launcher commands are supported, including status, stop and down.
# Registry authentication uses your existing Docker configuration.
exec bash "${CODER_DEV_DIR}/../../../dev.sh" "$@"
