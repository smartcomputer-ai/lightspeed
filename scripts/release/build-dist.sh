#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"
source release/metadata.env

version="${LIGHTSPEED_RELEASE_VERSION:-$LIGHTSPEED_PRODUCT_VERSION}"
git_sha="${LIGHTSPEED_GIT_SHA:-$(git rev-parse HEAD)}"
target="${LIGHTSPEED_RELEASE_TARGET}"
dist_dir="$repo_root/dist"

if [[ "$(rustc --version | awk '{print $2}')" != "$LIGHTSPEED_RELEASE_RUST_VERSION" ]]; then
  echo "release build requires Rust $LIGHTSPEED_RELEASE_RUST_VERSION" >&2
  exit 1
fi

rm -rf "$dist_dir"
mkdir -p "$dist_dir/bin" "$dist_dir/npm" "$dist_dir/contracts" \
  "$dist_dir/archives" "$dist_dir/configurator-mcp" "$dist_dir/runtime"

export LIGHTSPEED_RELEASE_VERSION="$version"
export LIGHTSPEED_GIT_SHA="$git_sha"
cargo build --release --locked --target "$target" \
  -p temporal-server -p environment-provider-incus -p environment-daemon -p cli

for binary in lightspeed-server lightspeed-provider-incus lightspeed-envd lightspeed; do
  install -m 0755 "target/$target/release/$binary" "$dist_dir/bin/$binary"
  strip "$dist_dir/bin/$binary"
done

cp crates/api/contract/api.schema.json crates/api/contract/methods.json \
  crates/api/contract/openrpc.json crates/api/contract/api-reference.md "$dist_dir/contracts/"

npm ci
npm run build

stage_root="$(mktemp -d)"
trap 'rm -rf "$stage_root"' EXIT
cp -R clients/typescript "$stage_root/ts-client"
rm -rf "$stage_root/ts-client/node_modules" "$stage_root/ts-client/dist"
node scripts/release/stage-package.mjs client "$stage_root/ts-client" "$version" "$git_sha"
(cd "$stage_root/ts-client" && npm ci --offline --ignore-scripts)
(cd "$stage_root/ts-client" && npm pack --pack-destination "$dist_dir/npm")

client_tgz="$(find "$dist_dir/npm" -maxdepth 1 -name '*.tgz' -print -quit)"
cp -R platform/configurator-mcp/dist "$dist_dir/configurator-mcp/dist"
cp platform/configurator-mcp/package.json platform/configurator-mcp/package-lock.json \
  "$dist_dir/configurator-mcp/"
cp "$client_tgz" "$dist_dir/configurator-mcp/agent-client.tgz"
node scripts/release/stage-package.mjs configurator "$dist_dir/configurator-mcp" "$version" "$git_sha"
(cd "$dist_dir/configurator-mcp" && \
  npm ci --omit=dev --offline --ignore-scripts)

scripts/release/stage-runtimes.sh "$dist_dir"

for spec in \
  "lightspeed-server:server" \
  "lightspeed-provider-incus:provider-incus" \
  "lightspeed-envd:envd" \
  "lightspeed:cli"; do
  binary="${spec%%:*}"
  asset="${spec##*:}"
  archive="lightspeed-${asset}-${version}-${target}.tar.gz"
  tar --sort=name --mtime='UTC 1970-01-01' --owner=0 --group=0 --numeric-owner \
    -C "$dist_dir/bin" -czf "$dist_dir/archives/$archive" "$binary"
done

scripts/release/create-sbom.mjs "$version" "$git_sha"
scripts/release/create-manifest.mjs "$version" "$git_sha"
scripts/release/checksums.sh
scripts/release/smoke.sh
