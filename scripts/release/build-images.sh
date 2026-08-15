#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

tag="${1:-lightspeed-local}"
source release/metadata.env
git_sha="${LIGHTSPEED_GIT_SHA:-$(git rev-parse HEAD)}"
version="${LIGHTSPEED_RELEASE_VERSION:-$LIGHTSPEED_PRODUCT_VERSION}"
docker build --file release/server.Dockerfile \
  --build-arg "LIGHTSPEED_RELEASE_VERSION=$version" \
  --build-arg "LIGHTSPEED_GIT_SHA=$git_sha" \
  --tag "${tag}-server" .
docker build --file release/configurator-mcp.Dockerfile \
  --build-arg "LIGHTSPEED_RELEASE_VERSION=$version" \
  --build-arg "LIGHTSPEED_GIT_SHA=$git_sha" \
  --tag "${tag}-configurator-mcp" .
docker build --file release/platform.Dockerfile \
  --build-arg "LIGHTSPEED_RELEASE_VERSION=$version" \
  --build-arg "LIGHTSPEED_GIT_SHA=$git_sha" \
  --tag "${tag}-platform" .
for role in workflows activities telegram; do
  docker build --file release/channels.Dockerfile \
    --build-arg "LIGHTSPEED_RELEASE_VERSION=$version" \
    --build-arg "LIGHTSPEED_GIT_SHA=$git_sha" \
    --build-arg "LIGHTSPEED_CHANNELS_ROLE=$role" \
    --build-arg LIGHTSPEED_CHANNELS_RUNTIME=channels \
    --tag "${tag}-channels-$role" .
done
docker build --file release/channels.Dockerfile \
  --build-arg "LIGHTSPEED_RELEASE_VERSION=$version" \
  --build-arg "LIGHTSPEED_GIT_SHA=$git_sha" \
  --build-arg LIGHTSPEED_CHANNELS_ROLE=whatsapp \
  --build-arg LIGHTSPEED_CHANNELS_RUNTIME=channels-whatsapp \
  --tag "${tag}-channels-whatsapp" .

container_id="$(docker create "${tag}-server" --version)"
trap 'docker rm -f "$container_id" >/dev/null 2>&1 || true' EXIT
tmp_dir="$(mktemp -d)"
trap 'docker rm -f "$container_id" >/dev/null 2>&1 || true; rm -rf "$tmp_dir"' EXIT
docker cp "$container_id:/usr/local/bin/lightspeed-server" "$tmp_dir/lightspeed-server"
cmp dist/bin/lightspeed-server "$tmp_dir/lightspeed-server"
docker run --rm "${tag}-server" --version
docker run --rm --entrypoint node "${tag}-configurator-mcp" -e \
  'for (const file of ["/app/dist/bin.js", "/app/agent-client.tgz"]) require("node:fs").accessSync(file)'
docker run --rm --entrypoint node "${tag}-platform" -e \
  'for (const file of ["/app/platform/server/src/main.ts", "/app/platform/web/dist/index.html"]) require("node:fs").accessSync(file)'
test "$(docker image inspect "${tag}-platform" \
  --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}')" = "$git_sha"
for role in workflows activities telegram whatsapp; do
  test "$(docker image inspect "${tag}-channels-$role" \
    --format '{{ index .Config.Labels "dev.lightspeed.channels.role" }}')" = "$role"
  test "$(docker image inspect "${tag}-channels-$role" \
    --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}')" = "$git_sha"
done
node -e 'JSON.parse(require("node:fs").readFileSync("dist/release-manifest.json", "utf8"))'
