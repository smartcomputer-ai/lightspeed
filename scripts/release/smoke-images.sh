#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

platform_image="${PLATFORM_IMAGE:?PLATFORM_IMAGE is required}"
expected_sha="${EXPECTED_SHA:?EXPECTED_SHA is required}"
declare -A channels_images=(
  [workflows]="${CHANNELS_WORKFLOWS_IMAGE:?CHANNELS_WORKFLOWS_IMAGE is required}"
  [activities]="${CHANNELS_ACTIVITIES_IMAGE:?CHANNELS_ACTIVITIES_IMAGE is required}"
  [telegram]="${CHANNELS_TELEGRAM_IMAGE:?CHANNELS_TELEGRAM_IMAGE is required}"
  [whatsapp]="${CHANNELS_WHATSAPP_IMAGE:?CHANNELS_WHATSAPP_IMAGE is required}"
)

docker pull "$platform_image"
for role in workflows activities telegram whatsapp; do
  docker pull "${channels_images[$role]}"
done

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

for role in workflows activities telegram whatsapp; do
  image="${channels_images[$role]}"
  test "$(docker image inspect "$image" \
    --format '{{ index .Config.Labels "dev.lightspeed.channels.role" }}')" = "$role"
  test "$(docker image inspect "$image" \
    --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}')" = "$expected_sha"
  docker run --rm --entrypoint node "$image" \
    --import tsx --input-type=module -e '
      const { resolveChannelsRoles } = await import("./platform/channels/src/runtime/roles.ts");
      const role = process.env.LIGHTSPEED_CHANNELS_ROLE;
      if (resolveChannelsRoles(role, undefined)[0] !== role) process.exit(1);
    '
  if [[ "$role" = whatsapp ]]; then
    docker run --rm --entrypoint node "$image" -e \
      'require("node:fs").accessSync("/app/node_modules/baileys/package.json")'
  else
    docker run --rm --entrypoint node "$image" -e \
      'if (require("node:fs").existsSync("/app/node_modules/baileys")) process.exit(1)'
  fi
done
