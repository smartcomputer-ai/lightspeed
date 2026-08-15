#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/common.sh"

# The runtime owns its migration ledger. Applying the SQL files directly would
# create an unledgered schema that the server correctly refuses to start.
export LIGHTSPEED_POSTGRES_URL
cd "${REPO_ROOT}"
cargo run -p temporal-server -- migrate
