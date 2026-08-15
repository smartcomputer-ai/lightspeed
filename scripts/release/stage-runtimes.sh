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
    platform/channels \
    platform/cli \
    platform/configurator-mcp \
    platform/db \
    platform/foundry \
    platform/server \
    platform/shared \
    platform/web; do
    mkdir -p "$root/$workspace"
    cp "$workspace/package.json" "$root/$workspace/"
  done
}

stage_runtime() {
  local name="$1"
  local workspace="$2"
  local optional_mode="$3"
  shift 3
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
  if [[ "$optional_mode" = omit ]]; then
    rm -rf "$root/node_modules/baileys" "$root/node_modules/qrcode-terminal"
    node --input-type=module -e '
      import fs from "node:fs";
      const file = process.argv[1];
      const manifest = JSON.parse(fs.readFileSync(file, "utf8"));
      delete manifest.dependencies.baileys;
      delete manifest.dependencies["qrcode-terminal"];
      fs.writeFileSync(file, `${JSON.stringify(manifest, null, 2)}\n`);
    ' "$root/platform/channels/package.json"
  fi
  rm -f "$root/package-lock.json"
  if [[ "$name" = platform ]]; then
    rm -rf "$root/platform/channels" "$root/platform/cli" \
      "$root/platform/configurator-mcp"
  else
    rm -rf "$root/platform/cli" "$root/platform/configurator-mcp" \
      "$root/platform/foundry" "$root/platform/server" \
      "$root/platform/shared" "$root/platform/web"
  fi
  "$tar_command" --sort=name --mtime='UTC 1970-01-01' --owner=0 --group=0 --numeric-owner \
    -C "$root" -czf "$dist_dir/runtime/$name.tar.gz" .
}

stage_runtime platform @lightspeed/platform-server include \
  clients/typescript/dist \
  platform/server/src \
  platform/db/src \
  platform/db/migrations \
  platform/shared/src \
  platform/foundry/src \
  platform/web/dist
stage_runtime channels @lightspeed/channels include \
  clients/typescript/dist \
  platform/channels/src \
  platform/db/src \
  platform/db/migrations
