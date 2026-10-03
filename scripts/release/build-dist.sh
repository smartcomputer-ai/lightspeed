#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"
source release/metadata.env

version="${LIGHTSPEED_RELEASE_VERSION:-$LIGHTSPEED_PRODUCT_VERSION}"
git_sha="${LIGHTSPEED_GIT_SHA:-$(git rev-parse HEAD)}"
target="${LIGHTSPEED_RELEASE_TARGET}"
envd_target="${LIGHTSPEED_ENVD_TARGET}"
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
# The server reports which daemon targets this release publishes.
export LIGHTSPEED_ENVD_TARGETS="$envd_target"
cargo build --release --locked --target "$target" \
  -p temporal-runtime -p environment-provider-incus -p cli
# The environment daemon alone is a static musl binary, so it runs on any
# Linux image whatever glibc that image carries.
cargo build --release --locked --target "$envd_target" -p environment-daemon

for binary in lightspeed-runtime lightspeed-provider-incus lightspeed; do
  install -m 0755 "target/$target/release/$binary" "$dist_dir/bin/$binary"
  strip "$dist_dir/bin/$binary"
done
install -m 0755 "target/$envd_target/release/lightspeed-envd" "$dist_dir/bin/lightspeed-envd"
strip "$dist_dir/bin/lightspeed-envd"

cp crates/api/contract/api.schema.json crates/api/contract/methods.json \
  crates/api/contract/openrpc.json crates/api/contract/api-reference.md "$dist_dir/contracts/"

npm ci
npm run build
npm run build:docs

# The publishable client and Configurator retain standalone lockfiles. Prime
# npm's cache from those exact locks before their staged installs go offline;
# the root workspace lock may legitimately resolve different transitive
# versions and therefore cannot guarantee that every standalone tarball is
# cached.
npm --prefix clients/typescript ci --ignore-scripts
npm --prefix platform/configurator-mcp ci --ignore-scripts

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
cp "$client_tgz" "$dist_dir/configurator-mcp/sdk.tgz"
node scripts/release/stage-package.mjs configurator "$dist_dir/configurator-mcp" "$version" "$git_sha"
(cd "$dist_dir/configurator-mcp" && \
  npm ci --omit=dev --offline --ignore-scripts)

scripts/release/stage-runtimes.sh "$dist_dir"

for spec in \
  "lightspeed-runtime:runtime:$target" \
  "lightspeed-provider-incus:provider-incus:$target" \
  "lightspeed-envd:envd:$envd_target" \
  "lightspeed:cli:$target"; do
  IFS=: read -r binary asset archive_target <<<"$spec"
  archive="lightspeed-${asset}-${version}-${archive_target}.tar.gz"
  tar --sort=name --mtime='UTC 1970-01-01' --owner=0 --group=0 --numeric-owner \
    -C "$dist_dir/bin" -czf "$dist_dir/archives/$archive" "$binary"
done

scripts/release/stage-static-sites.sh "$dist_dir"

scripts/release/create-sbom.mjs "$version" "$git_sha"
scripts/release/create-manifest.mjs "$version" "$git_sha"
scripts/release/checksums.sh
scripts/release/smoke.sh
