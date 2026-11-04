#!/usr/bin/env bash
# Says whether an upgrade from <from-ref> can restart on its existing Temporal
# namespace. Retained workflow histories replay on the new code, so any change to
# the workflow crate, its workspace dependencies or the Temporal SDK versions
# counts as breaking. This is a diff, not a replay check: a breaking verdict can be
# a false alarm, and a release with one gets a new namespace. It sees no other
# third-party version, so read the Cargo.lock diff before trusting restart-in-place.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "usage: $0 <from-ref> [to-ref, default HEAD]" >&2
  exit 2
fi
from=$(git rev-parse --verify "$1^{commit}")
to=$(git rev-parse --verify "${2:-HEAD}^{commit}")

# temporal-workflow and every workspace crate it depends on.
crates=(crates/temporal-workflow crates/harness crates/api crates/bots crates/channels)

# A version bump leaves the package's name line unchanged, so compare whole
# package stanzas rather than grepping the diff for temporalio.
temporal_packages() {
  git show "$1:Cargo.lock" | awk 'BEGIN { RS = ""; ORS = "\n\n" } /\nname = "temporalio/'
}

crate_diff=$(git diff --stat "$from" "$to" -- "${crates[@]}")
sdk_diff=$(diff <(temporal_packages "$from") <(temporal_packages "$to") || true)

if [[ -n "$crate_diff" ]]; then
  printf '%s\n\n' "$crate_diff"
fi
if [[ -n "$sdk_diff" ]]; then
  printf 'Temporal SDK packages in Cargo.lock:\n%s\n\n' "$sdk_diff"
fi
if [[ -n "$crate_diff" || -n "$sdk_diff" ]]; then
  echo "breaking"
else
  echo "restart-in-place"
fi
