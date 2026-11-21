#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

tag="${1:-lightspeed-local}"
source release/metadata.env
git_sha="${LIGHTSPEED_GIT_SHA:-$(git rev-parse HEAD)}"
version="${LIGHTSPEED_RELEASE_VERSION:-$LIGHTSPEED_PRODUCT_VERSION}"
docker build --file release/runtime.Dockerfile \
  --build-arg "LIGHTSPEED_RELEASE_VERSION=$version" \
  --build-arg "LIGHTSPEED_GIT_SHA=$git_sha" \
  --tag "${tag}-runtime" .
docker build --file release/configurator-mcp.Dockerfile \
  --build-arg "LIGHTSPEED_RELEASE_VERSION=$version" \
  --build-arg "LIGHTSPEED_GIT_SHA=$git_sha" \
  --tag "${tag}-configurator-mcp" .
docker build --file release/platform.Dockerfile \
  --build-arg "LIGHTSPEED_RELEASE_VERSION=$version" \
  --build-arg "LIGHTSPEED_GIT_SHA=$git_sha" \
  --tag "${tag}-platform" .

container_id="$(docker create "${tag}-runtime" --version)"
trap 'docker rm -f "$container_id" >/dev/null 2>&1 || true' EXIT
tmp_dir="$(mktemp -d)"
trap 'docker rm -f "$container_id" >/dev/null 2>&1 || true; rm -rf "$tmp_dir"' EXIT
docker cp "$container_id:/usr/local/bin/lightspeed-runtime" "$tmp_dir/lightspeed-runtime"
cmp dist/bin/lightspeed-runtime "$tmp_dir/lightspeed-runtime"
docker cp "$container_id:/usr/local/bin/lightspeed" "$tmp_dir/lightspeed"
cmp dist/bin/lightspeed "$tmp_dir/lightspeed"
docker run --rm "${tag}-runtime" --version
# The CLI of the same release, run as the image's user with no home directory.
docker run --rm --entrypoint /usr/local/bin/lightspeed "${tag}-runtime" --version
docker run --rm --entrypoint /usr/local/bin/lightspeed "${tag}-runtime" config schema > /dev/null
docker run --rm --entrypoint node "${tag}-configurator-mcp" \
  --input-type=module -e 'await import("/app/dist/index.js")'
docker run --rm --entrypoint node "${tag}-platform" -e \
  'for (const file of ["/app/distributions/standard/server.ts", "/app/platform/web/dist/index.html"]) require("node:fs").accessSync(file)'
for image in runtime platform; do
  test "$(docker image inspect "${tag}-${image}" \
    --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}')" = "$git_sha"
done
