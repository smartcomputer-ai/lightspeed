#!/usr/bin/env bash
set -euo pipefail

LIGHTSPEED_DEV_LAUNCHER_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${LIGHTSPEED_DEV_LAUNCHER_DIR}"

fail() {
  echo "dev.sh: $*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "$1 is required; install it and retry"
}

require_command node
node_major="$(node -p 'Number(process.versions.node.split(".")[0])')"
if [[ ! "${node_major}" =~ ^[0-9]+$ ]] || (( node_major < 24 )); then
  fail "Node.js 24 or newer is required (found $(node --version))"
fi

action="start"
profile="full"
plan_only=false
help_only=false
positionals=()
for argument in "$@"; do
  case "${argument}" in
    --plan) plan_only=true ;;
    --help|-h) help_only=true ;;
    --volumes|-v) ;;
    *) positionals+=("${argument}") ;;
  esac
done

if (( ${#positionals[@]} > 0 )); then
  case "${positionals[0]}" in
    start|stop|down|reset|status)
      action="${positionals[0]}"
      if [[ "${action}" == "start" && ${#positionals[@]} -gt 1 ]]; then
        profile="${positionals[1]}"
      fi
      ;;
    full|platform|runtime|infra)
      profile="${positionals[0]}"
      ;;
  esac
fi

need_docker=false
need_cargo=false
need_node_dependencies=false
if [[ "${help_only}" != true && "${plan_only}" != true ]]; then
  case "${action}" in
    start)
      need_docker=true
      case "${profile}" in
        full)
          need_cargo=true
          need_node_dependencies=true
          ;;
        platform) need_node_dependencies=true ;;
        runtime) need_cargo=true ;;
      esac
      ;;
    status|down) need_docker=true ;;
    reset)
      need_docker=true
      need_cargo=true
      ;;
  esac
fi

if [[ "${need_docker}" == true ]]; then
  require_command docker
  docker compose version >/dev/null 2>&1 || fail "Docker Compose v2 is required"
  docker info >/dev/null 2>&1 || fail "the Docker daemon is not available"
fi

if [[ "${need_cargo}" == true ]]; then
  require_command cargo
fi

if [[ "${need_node_dependencies}" == true ]]; then
  require_command npm
  lock_hash="$(node -e '
    const fs = require("node:fs");
    const crypto = require("node:crypto");
    process.stdout.write(crypto.createHash("sha256").update(fs.readFileSync(process.argv[1])).digest("hex"));
  ' package-lock.json)"
  stamp_path=".lightspeed/dev-npm-lock.sha256"
  stamped_hash=""
  if [[ -f "${stamp_path}" ]]; then
    stamped_hash="$(<"${stamp_path}")"
  fi
  if [[ ! -x node_modules/.bin/tsx || ! -x node_modules/.bin/vite || "${stamped_hash}" != "${lock_hash}" ]]; then
    echo "[bootstrap] Installing root npm workspace dependencies..."
    npm install
    mkdir -p "$(dirname "${stamp_path}")"
    lock_hash="$(node -e '
      const fs = require("node:fs");
      const crypto = require("node:crypto");
      process.stdout.write(crypto.createHash("sha256").update(fs.readFileSync(process.argv[1])).digest("hex"));
    ' package-lock.json)"
    printf '%s\n' "${lock_hash}" > "${stamp_path}"
  fi
fi

exec node scripts/dev/stack.mjs "$@"
