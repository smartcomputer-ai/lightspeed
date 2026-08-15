#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
source release/metadata.env

if [[ ! "$LIGHTSPEED_PRODUCT_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
  echo "LIGHTSPEED_PRODUCT_VERSION must be an OCI-compatible SemVer without +build metadata" >&2
  exit 1
fi

grep -F "channel = \"$LIGHTSPEED_RELEASE_RUST_VERSION\"" rust-toolchain.toml >/dev/null
grep -F "version = \"$LIGHTSPEED_PRODUCT_VERSION\"" crates/release-info/Cargo.toml >/dev/null
grep -F "pub const REQUIRED_SCHEMA_REVISION: i64 = $LIGHTSPEED_SCHEMA_REVISION;" \
  crates/store-pg/src/migrations.rs >/dev/null
grep -F "pub const PROTOCOL_VERSION: &str = \"$LIGHTSPEED_API_PROTOCOL_VERSION\";" \
  crates/api/src/constants.rs >/dev/null
grep -F "$LIGHTSPEED_RELEASE_BUILD_BASE_IMAGE" release/build-env.Dockerfile >/dev/null
node -e '
  const fs = require("node:fs");
  const journal = JSON.parse(fs.readFileSync("platform/db/migrations/meta/_journal.json", "utf8"));
  const revision = Number(process.argv[1]);
  const baseline = process.argv[2];
  if (journal.entries.length !== revision) throw new Error("platform schema revision is stale");
  if (!journal.entries.some((entry) => entry.tag === baseline)) {
    throw new Error("platform upgrade baseline is not in the migration journal");
  }
' "$LIGHTSPEED_PLATFORM_SCHEMA_REVISION" "$LIGHTSPEED_PLATFORM_UPGRADE_FROM"

for manifest in \
  crates/release-info/Cargo.toml \
  crates/temporal-server/Cargo.toml \
  crates/environment-provider-incus/Cargo.toml \
  crates/environment-daemon/Cargo.toml \
  crates/cli/Cargo.toml; do
  grep -F "version = \"$LIGHTSPEED_PRODUCT_VERSION\"" "$manifest" >/dev/null
done

if rg -n 'uses: [^@[:space:]]+@(master|main|v[0-9])' .github/workflows; then
  echo "GitHub Actions must be pinned to full commit SHAs" >&2
  exit 1
fi
