#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/../lib/common.sh"

namespace="${TEMPORAL_NAMESPACE:-default}"
attributes=(
  LightspeedUniverseId
  LightspeedBotTriggerId
  LightspeedBotId
)

ready=false
attributes_json=""
for _ in {1..60}; do
  if attributes_json="$(
    compose exec -T temporal temporal operator search-attribute list \
      --namespace "${namespace}" \
      --output json \
      2>/dev/null
  )"; then
    ready=true
    break
  fi
  sleep 1
done

if [[ "${ready}" != true ]]; then
  echo "Temporal namespace ${namespace} did not become ready for search-attribute registration." >&2
  compose exec -T temporal temporal operator search-attribute list \
    --namespace "${namespace}" \
    --output json >&2
  exit 1
fi

for attribute in "${attributes[@]}"; do
  if grep -Fq "\"${attribute}\": \"INDEXED_VALUE_TYPE_KEYWORD\"" <<<"${attributes_json}"; then
    continue
  fi
  if grep -Fq "\"${attribute}\":" <<<"${attributes_json}"; then
    echo "Temporal search attribute ${attribute} exists with a type other than Keyword." >&2
    exit 1
  fi
  compose exec -T temporal temporal operator search-attribute create \
    --namespace "${namespace}" \
    --name "${attribute}" \
    --type Keyword
done

echo "Temporal search attributes are ready."
