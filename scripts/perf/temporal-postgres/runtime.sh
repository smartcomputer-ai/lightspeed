#!/usr/bin/env bash
# Launch the isolated performance runtime with enough descriptors for readers.
set -euo pipefail
cd "$(dirname "$0")/../../.."
source scripts/dev/env.sh >/dev/null
source scripts/perf/temporal-postgres/env.sh
fds=${PERF_RUNTIME_NOFILE:-8192}
[[ "$fds" =~ ^[0-9]+$ ]] && (( fds >= 1024 && fds <= 65536 )) || exit 1
ulimit -Sn "$fds"
exec "${PERF_RUNTIME_BINARY:-target/release/lightspeed-runtime}"
