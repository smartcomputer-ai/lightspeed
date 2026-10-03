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
  local workspace
  cp package.json package-lock.json tsconfig.json "$root/"
  for workspace in \
    clients/typescript \
    docs/site \
    platform/cli \
    platform/configurator-mcp \
    platform/connectors \
    platform/db \
    platform/backend \
    platform/shared \
    platform/web; do
    mkdir -p "$root/$workspace"
    cp "$workspace/package.json" "$root/$workspace/"
  done
}

stage_runtime() {
  local name="$1"
  local workspace="$2"
  shift 2
  local root="$stage_root/runtime-$name"
  local source
  local -a install_args=(ci --workspace "$workspace" --omit=dev --offline --ignore-scripts)

  mkdir -p "$root"
  copy_workspace_manifests "$root"
  for source in "$@"; do
    mkdir -p "$root/$(dirname "$source")"
    cp -R "$source" "$root/$source"
  done
  (cd "$root" && npm "${install_args[@]}")
  rm -f "$root/package-lock.json"
  # The documentation workspace participates in dependency resolution only.
  rm -rf "$root/docs"
  if [[ "$name" = platform ]]; then
    rm -rf "$root/platform/cli" "$root/platform/configurator-mcp" \
      "$root/platform/connectors"
  else
    rm -rf "$root/platform/cli" "$root/platform/configurator-mcp" \
      "$root/platform/db" "$root/platform/backend" "$root/platform/shared" \
      "$root/platform/web"
  fi
  "$tar_command" --sort=name --mtime='UTC 1970-01-01' --owner=0 --group=0 --numeric-owner \
    -C "$root" -czf "$dist_dir/runtime/$name.tar.gz" .
}

stage_runtime platform @lightspeed-ai/platform-backend \
  clients/typescript/dist \
  platform/backend/src \
  platform/db/src \
  platform/db/migrations \
  platform/shared/src \
  platform/web/dist
# The "platform-workers" runtime is the connector host (platform/connectors):
# Bots and Channels core moved into the Rust runtime. The artifact keeps its
# name so image references and manifest keys stay stable.
stage_runtime platform-workers @lightspeed-ai/connectors \
  clients/typescript/dist \
  platform/connectors/src
