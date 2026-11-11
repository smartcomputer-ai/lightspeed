#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
if [[ -x "$example_dir/.venv/bin/python" ]]; then
  exec "$example_dir/.venv/bin/python" "$example_dir/bootstrap.py" "$@"
fi
exec uv run --locked --project "$example_dir" python "$example_dir/bootstrap.py" "$@"
