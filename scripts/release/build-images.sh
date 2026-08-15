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
docker build --file release/channels.Dockerfile \
  --build-arg "LIGHTSPEED_RELEASE_VERSION=$version" \
  --build-arg "LIGHTSPEED_GIT_SHA=$git_sha" \
  --tag "${tag}-channels" .

container_id="$(docker create "${tag}-runtime" --version)"
trap 'docker rm -f "$container_id" >/dev/null 2>&1 || true' EXIT
tmp_dir="$(mktemp -d)"
trap 'docker rm -f "$container_id" >/dev/null 2>&1 || true; rm -rf "$tmp_dir"' EXIT
docker cp "$container_id:/usr/local/bin/lightspeed-server" "$tmp_dir/lightspeed-server"
cmp dist/bin/lightspeed-server "$tmp_dir/lightspeed-server"
docker run --rm "${tag}-runtime" --version
docker run --rm --entrypoint node "${tag}-configurator-mcp" -e \
  'for (const file of ["/app/dist/bin.js", "/app/agent-client.tgz"]) require("node:fs").accessSync(file)'
docker run --rm --entrypoint node "${tag}-platform" -e \
  'for (const file of ["/app/platform/server/src/main.ts", "/app/platform/web/dist/index.html"]) require("node:fs").accessSync(file)'
for image in runtime platform channels; do
  test "$(docker image inspect "${tag}-${image}" \
    --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}')" = "$git_sha"
done
test "$(docker image inspect "${tag}-channels" --format '{{json .Config.Cmd}}')" = '["all"]'
docker run --rm --entrypoint node "${tag}-channels" -e \
  'require("node:fs").accessSync("/app/node_modules/baileys/package.json")'
for role in workflows activities telegram whatsapp all; do
  expected="$role"
  if [[ "$role" = all ]]; then
    expected=workflows,activities,telegram
  fi
  docker run --rm --entrypoint node -e "TEST_ROLE=$role" -e "TEST_EXPECTED=$expected" \
    "${tag}-channels" \
    --import tsx --input-type=module -e '
      const { resolveChannelsRoles } = await import("./platform/channels/src/runtime/roles.ts");
      const actual = resolveChannelsRoles(process.env.TEST_ROLE, undefined).join(",");
      if (actual !== process.env.TEST_EXPECTED) process.exit(1);
    '
done
node -e 'JSON.parse(require("node:fs").readFileSync("dist/release-manifest.json", "utf8"))'
