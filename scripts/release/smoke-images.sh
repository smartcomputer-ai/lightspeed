#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

platform_image="${PLATFORM_IMAGE:?PLATFORM_IMAGE is required}"
channels_image="${CHANNELS_IMAGE:?CHANNELS_IMAGE is required}"
expected_sha="${EXPECTED_SHA:?EXPECTED_SHA is required}"

docker pull "$platform_image"
docker pull "$channels_image"

test "$(docker image inspect "$platform_image" \
  --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}')" = "$expected_sha"

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

test "$(docker image inspect "$channels_image" \
  --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}')" = "$expected_sha"
test "$(docker image inspect "$channels_image" --format '{{json .Config.Cmd}}')" = '["all"]'
docker run --rm --entrypoint node "$channels_image" -e \
  'require("node:fs").accessSync("/app/node_modules/baileys/package.json")'

for role in workflows activities telegram whatsapp all; do
  expected="$role"
  if [[ "$role" = all ]]; then
    expected=workflows,activities,telegram
  fi
  docker run --rm --entrypoint node -e "TEST_ROLE=$role" -e "TEST_EXPECTED=$expected" \
    "$channels_image" \
    --import tsx --input-type=module -e '
      const { resolveChannelsRoles } = await import("./platform/channels/src/runtime/roles.ts");
      const actual = resolveChannelsRoles(process.env.TEST_ROLE, undefined).join(",");
      if (actual !== process.env.TEST_EXPECTED) process.exit(1);
    '
done
