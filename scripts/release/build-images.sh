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
docker build --file release/platform-workers.Dockerfile \
  --build-arg "LIGHTSPEED_RELEASE_VERSION=$version" \
  --build-arg "LIGHTSPEED_GIT_SHA=$git_sha" \
  --tag "${tag}-platform-workers" .

container_id="$(docker create "${tag}-runtime" --version)"
trap 'docker rm -f "$container_id" >/dev/null 2>&1 || true' EXIT
tmp_dir="$(mktemp -d)"
trap 'docker rm -f "$container_id" >/dev/null 2>&1 || true; rm -rf "$tmp_dir"' EXIT
docker cp "$container_id:/usr/local/bin/lightspeed-runtime" "$tmp_dir/lightspeed-runtime"
cmp dist/bin/lightspeed-runtime "$tmp_dir/lightspeed-runtime"
docker run --rm "${tag}-runtime" --version
docker run --rm --entrypoint node "${tag}-configurator-mcp" \
  --input-type=module -e 'await import("/app/dist/index.js")'
docker run --rm --entrypoint node "${tag}-platform" -e \
  'for (const file of ["/app/platform/backend/src/main.ts", "/app/platform/web/dist/index.html"]) require("node:fs").accessSync(file)'
for image in runtime platform platform-workers; do
  test "$(docker image inspect "${tag}-${image}" \
    --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}')" = "$git_sha"
done
docker run --rm --entrypoint node "${tag}-platform-workers" -e \
  'for (const file of ["/app/node_modules/baileys/package.json", "/app/platform/connectors/src/host/main.ts"]) require("node:fs").accessSync(file)'
# The connector host parses its configuration and derives account task queues
# from the generated client; both must load from the staged runtime.
docker run --rm --entrypoint node "${tag}-platform-workers" \
  --import tsx --input-type=module -e '
    const { parseHostConfig } = await import("./platform/connectors/src/host/config.ts");
    const { connectorTaskQueue, WORKFLOW_CONTRACT_VECTORS } = await import("@lightspeed-ai/client/workflow");
    const config = parseHostConfig({ LIGHTSPEED_API_URL: "http://runtime:18080/rpc", LIGHTSPEED_CONNECTOR_PROVIDERS: "telegram" });
    if (config.providers.join(",") !== "telegram") process.exit(1);
    const vector = WORKFLOW_CONTRACT_VECTORS.channels;
    if (connectorTaskQueue(vector.inputs.universeId, vector.inputs.provider, vector.inputs.accountId) !== vector.connectorTaskQueue) process.exit(1);
  '
node -e 'JSON.parse(require("node:fs").readFileSync("dist/release-manifest.json", "utf8"))'
