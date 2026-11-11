#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

find dist/archives dist/npm dist/contracts dist/bin dist/provenance dist/sbom.spdx.json dist/envd.json -type f -print0 \
  | sort -z \
  | xargs -0 sha256sum \
  | sed 's#  dist/#  #' > dist/checksums.txt
