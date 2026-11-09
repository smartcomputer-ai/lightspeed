#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

platform_image="${PLATFORM_IMAGE:?PLATFORM_IMAGE is required}"
configurator_image="${CONFIGURATOR_IMAGE:?CONFIGURATOR_IMAGE is required}"
platform_workers_image="${PLATFORM_WORKERS_IMAGE:?PLATFORM_WORKERS_IMAGE is required}"
expected_sha="${EXPECTED_SHA:?EXPECTED_SHA is required}"

docker pull "$platform_image"
docker pull "$configurator_image"
docker pull "$platform_workers_image"

# Check both the image metadata used by Kubernetes and the effective runtime IDs.
for image in "$platform_image" "$configurator_image" "$platform_workers_image"; do
  image_user="$(docker image inspect "$image" --format '{{.Config.User}}')"
  if [[ "$image_user" != '1000:1000' ]]; then
    echo "$image must declare numeric USER 1000:1000; found '$image_user'" >&2
    exit 1
  fi
  docker run --rm --entrypoint node "$image" --input-type=module -e '
    import assert from "node:assert/strict";
    assert.equal(process.getuid(), 1000, "runtime UID");
    assert.equal(process.getgid(), 1000, "runtime GID");
  '
done

test "$(docker image inspect "$platform_image" \
  --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}')" = "$expected_sha"

docker run --rm --entrypoint node "$platform_image" --input-type=module -e '
  import { existsSync, readFileSync, readdirSync } from "node:fs";
  const forbidden = new Set(["vitest", "vite", "jsdom", "drizzle-kit", "@playwright/test", "screendiff-js"]);
  const lock = JSON.parse(readFileSync("/app/node_modules/.package-lock.json", "utf8"));
  for (const [location, item] of Object.entries(lock.packages)) {
    const name = item.name ?? location.split("node_modules/").at(-1);
    if (forbidden.has(name)) throw new Error(`Development dependency shipped in platform: ${location}`);
  }
  for (const file of ["/usr/local/lib/node_modules/npm", "/usr/local/bin/npm", "/usr/local/bin/npx", "/usr/local/bin/yarn", "/usr/local/bin/yarnpkg"]) {
    if (existsSync(file)) throw new Error(`Package manager shipped in platform: ${file}`);
  }
  if (readdirSync("/opt").some(name => name.startsWith("yarn-"))) throw new Error("Yarn shipped in platform");
'

platform_container="$(docker run --detach --network host \
  -e PORT=18300 \
  -e LIGHTSPEED_PLATFORM_DATABASE_URL="${PLATFORM_DATABASE_URL:?PLATFORM_DATABASE_URL is required}" \
  -e LIGHTSPEED_PLATFORM_AUTH_SECRET=lightspeed-release-smoke-auth-secret \
  -e LIGHTSPEED_PLATFORM_BASE_URL=http://127.0.0.1:18300 \
  "$platform_image")"
cleanup() {
  docker rm -f "$platform_container" >/dev/null 2>&1 || true
}
trap cleanup EXIT

ready=false
for _ in $(seq 1 30); do
  if curl --fail --silent http://127.0.0.1:18300/health \
    | grep -F '"ok":true' >/dev/null; then
    ready=true
    break
  fi
  sleep 1
done
if [[ "$ready" != true ]]; then
  docker logs "$platform_container" >&2
  echo "platform image did not become healthy" >&2
  exit 1
fi
curl --fail --silent http://127.0.0.1:18300/app \
  | grep -F '<div id="root"></div>' >/dev/null

test "$(docker image inspect "$configurator_image" \
  --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}')" = "$expected_sha"
docker run --rm --entrypoint node "$configurator_image" \
  --input-type=module -e 'await import("/app/dist/index.js")'

test "$(docker image inspect "$platform_workers_image" \
  --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}')" = "$expected_sha"
docker run --rm --entrypoint node "$platform_workers_image" -e \
  'for (const file of ["/app/node_modules/baileys/package.json", "/app/platform/connectors/src/host/main.ts"]) require("node:fs").accessSync(file)'
docker run --rm --entrypoint node "$platform_workers_image" \
  --import tsx --input-type=module -e '
    const { parseHostConfig } = await import("./platform/connectors/src/host/config.ts");
    const { connectorTaskQueue, WORKFLOW_CONTRACT_VECTORS } = await import("@lightspeed-ai/sdk/workflow");
    const config = parseHostConfig({
      LIGHTSPEED_API_URL: "http://runtime:18080/rpc",
      LIGHTSPEED_CONNECTOR_API_KEY: "lsk_release_smoke_test",
      LIGHTSPEED_CONNECTOR_PROVIDERS: "telegram",
    });
    if (config.providers.join(",") !== "telegram") process.exit(1);
    const vector = WORKFLOW_CONTRACT_VECTORS.channels;
    if (connectorTaskQueue(vector.inputs.universeId, vector.inputs.provider, vector.inputs.accountId) !== vector.connectorTaskQueue) process.exit(1);
  '
