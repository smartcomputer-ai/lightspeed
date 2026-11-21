#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"
dist_dir="${1:-$repo_root/dist}"
mkdir -p "$dist_dir/runtime"
tar_command=tar
if command -v gtar >/dev/null 2>&1; then
  tar_command=gtar
fi

stage_root="$(mktemp -d)"
trap 'rm -rf "$stage_root"' EXIT

copy_workspace_manifests() {
  local root="$1"
  cp package.json package-lock.json tsconfig.json "$root/"
  node --input-type=module - "$root" <<'JS'
import { copyFileSync, globSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import pkg from './package.json' with { type: 'json' };

for (const manifest of globSync(pkg.workspaces.map(pattern => `${pattern}/package.json`))) {
  const destination = join(process.argv[2], manifest);
  mkdirSync(dirname(destination), { recursive: true });
  copyFileSync(manifest, destination);
}
JS
}

stage_runtime() {
  local name="$1"
  local workspace="$2"
  shift 2
  local root="$stage_root/runtime-$name"
  local source

  mkdir -p "$root"
  copy_workspace_manifests "$root"
  for source in "$@"; do
    mkdir -p "$root/$(dirname "$source")"
    cp -R "$source" "$root/$source"
  done
  node scripts/release/install-runtime-dependencies.mjs "$root" "$workspace"
  rm -f "$root/package-lock.json"
  # The documentation workspace participates in dependency resolution only.
  rm -rf "$root/docs"
  rm -rf "$root/platform/configurator-mcp"
  "$tar_command" --sort=name --mtime='UTC 1970-01-01' --owner=0 --group=0 --numeric-owner \
    -C "$root" -czf "$dist_dir/runtime/$name.tar.gz" .
}

# A distribution can ship just the host, with no module descriptors to copy.
shopt -s nullglob
stage_runtime platform @lightspeed-ai/platform-backend \
  clients/typescript/dist \
  distributions/standard/api.ts \
  distributions/standard/catalog.ts \
  distributions/standard/server.ts \
  modules/*/api \
  modules/*/module.ts \
  platform/backend/src \
  platform/db/src \
  platform/db/migrations \
  platform/shared/src \
  platform/web/dist
