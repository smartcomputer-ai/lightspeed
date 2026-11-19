#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
source release/metadata.env

# uv builds the wheel from the sdist, so missing source files fail here.
out_dir=${1:?usage: build-python-sdk.sh OUTPUT_DIRECTORY}
mkdir -p "$out_dir"
out_dir=$(cd "$out_dir" && pwd)
uv build --python 3.12 --no-sources --out-dir "$out_dir" clients/python

for file in "lightspeed_ai-${LIGHTSPEED_PRODUCT_VERSION}.tar.gz" \
            "lightspeed_ai-${LIGHTSPEED_PRODUCT_VERSION}-py3-none-any.whl"; do
  uv run --python 3.12 --isolated --no-project --with "$out_dir/$file" python -I -c '
import sys
from importlib.metadata import version
from lightspeed_ai import AsyncLightspeedClient

if version("lightspeed-ai") != sys.argv[1]:
    sys.exit("Installed SDK version differs from the release version")
' "$LIGHTSPEED_PRODUCT_VERSION"
done
