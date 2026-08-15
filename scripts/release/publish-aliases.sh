#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

if [[ "$#" -ne 2 ]]; then
  echo "usage: publish-aliases.sh <alias> <release-bundle-digest>" >&2
  exit 2
fi

alias_name="$1"
bundle_digest="$2"
root="${GHCR_ROOT:?GHCR_ROOT is required}"
manifest="dist/release-manifest.json"

if [[ ! "$alias_name" =~ ^(sha-[0-9a-f]{40}|[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?)$ ]]; then
  echo "invalid snapshot or release alias: $alias_name" >&2
  exit 1
fi
if [[ ! "$bundle_digest" =~ ^sha256:[0-9a-f]{64}$ ]]; then
  echo "release bundle digest must be a sha256 digest" >&2
  exit 1
fi

scripts/release/verify-manifest.mjs --published

manifest_sha="$(jq -er .gitSha "$manifest")"
manifest_version="$(jq -er .version "$manifest")"
if [[ "$alias_name" == sha-* ]]; then
  test "$alias_name" = "sha-$manifest_sha" || {
    echo "$alias_name does not match manifest commit $manifest_sha" >&2
    exit 1
  }
else
  test "$alias_name" = "$manifest_version" || {
    echo "$alias_name does not match manifest version $manifest_version" >&2
    exit 1
  }
fi

copy_image_alias() {
  local source_ref="$1"
  local target_ref="$2"
  local expected_digest existing_digest

  [[ "$source_ref" =~ @sha256:[0-9a-f]{64}$ ]]
  expected_digest="$(crane digest "$source_ref")"
  if existing_digest="$(crane digest "$target_ref" 2>/dev/null)"; then
    test "$existing_digest" = "$expected_digest" || {
      echo "$target_ref already points at $existing_digest, not $expected_digest" >&2
      return 1
    }
  else
    crane cp "$source_ref" "$target_ref"
    test "$(crane digest "$target_ref")" = "$expected_digest"
  fi
}

copy_oras_alias() {
  local source_ref="$1"
  local target_ref="$2"
  local expected_digest existing_digest

  [[ "$source_ref" =~ @sha256:[0-9a-f]{64}$ ]]
  expected_digest="$(oras resolve "$source_ref")"
  if existing_digest="$(oras resolve "$target_ref" 2>/dev/null)"; then
    test "$existing_digest" = "$expected_digest" || {
      echo "$target_ref already points at $existing_digest, not $expected_digest" >&2
      return 1
    }
  else
    oras copy "$source_ref" "$target_ref"
    test "$(oras resolve "$target_ref")" = "$expected_digest"
  fi
}

if [[ "$alias_name" == sha-* ]]; then
  # A snapshot has one public identity. Its manifest supplies the exact image
  # and binary digests, so component SHA tags would only create partial state.
  copy_oras_alias "$root/release-bundle@$bundle_digest" \
    "$root/release-bundle:$alias_name"
  exit 0
fi

copy_image_alias "$(jq -er .buildImage "$manifest")" \
  "$root/build-env:$alias_name"
copy_image_alias "$(jq -er .images.server "$manifest")" \
  "$root/server:$alias_name"
copy_image_alias "$(jq -er .images.configuratorMcp "$manifest")" \
  "$root/configurator-mcp:$alias_name"
copy_image_alias "$(jq -er .images.platform "$manifest")" \
  "$root/platform:$alias_name"
copy_image_alias "$(jq -er .images.channelsWorkflows "$manifest")" \
  "$root/channels-workflows:$alias_name"
copy_image_alias "$(jq -er .images.channelsActivities "$manifest")" \
  "$root/channels-activities:$alias_name"
copy_image_alias "$(jq -er .images.channelsTelegram "$manifest")" \
  "$root/channels-telegram:$alias_name"
copy_image_alias "$(jq -er .images.channelsWhatsapp "$manifest")" \
  "$root/channels-whatsapp:$alias_name"

while IFS=$'\t' read -r manifest_key target_name; do
  source_url="$(jq -er --arg key "$manifest_key" '.binaries[$key].url' "$manifest")"
  source_ref="${source_url#oci://}"
  [[ "$source_ref" != "$source_url" ]]
  copy_oras_alias "$source_ref" "$root/${target_name}-bundle:$alias_name"
done <<'EOF'
server	server
providerIncus	provider-incus
envd	envd
cli	cli
EOF

# This is also the completion marker for the official OCI artifact set.
copy_oras_alias "$root/release-bundle@$bundle_digest" \
  "$root/release-bundle:$alias_name"
