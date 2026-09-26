#!/usr/bin/env bash
set -Eeuo pipefail

LIGHTSPEED_DEV_LAUNCHER_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${LIGHTSPEED_DEV_LAUNCHER_DIR}"

fail() {
  printf '\ndev.sh: %s\n' "$1" >&2
  if [[ -n "${2:-}" ]]; then printf '\nNext: %s\n' "$2" >&2; fi
  exit 1
}

bootstrap_step="checking local tools"
trap 'fail "Failed while ${bootstrap_step} (shell line ${LINENO})." "Review the command output above, fix the reported error, and retry."' ERR

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "$1 was not found on PATH." "Install $1 and make it available in this terminal, then retry."
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
    --allow-missing-api-keys|--require-api-keys|--no-envd|--debug) ;;
    --volumes|-v) ;;
    -*) fail "Unknown option: ${argument}" "Run ./dev.sh --help for supported options." ;;
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
    full|platform|runtime|demo|docs|doc|documentation|infra)
      profile="${positionals[0]}"
      ;;
    *) fail "Unknown command or profile: ${positionals[0]}" "Run ./dev.sh --help for supported commands." ;;
  esac
fi

case "${profile}" in
  full|platform|runtime|demo|docs|doc|documentation|infra) ;;
  *) fail "Unknown profile: ${profile}" "Choose full, platform, runtime, demo, docs, or infra." ;;
esac
max_positionals=1
if [[ "${positionals[0]:-}" == start ]]; then max_positionals=2; fi
if (( ${#positionals[@]} > max_positionals )); then
  fail "Too many command arguments." "Run ./dev.sh --help for supported commands."
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
        demo|docs|doc|documentation)
          # Standalone web previews: nothing to run in Docker.
          need_docker=false
          need_node_dependencies=true
          ;;
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
  bootstrap_step="checking Docker"
  require_command docker
  if ! docker_check="$(docker compose version 2>&1)"; then
    printf '%s\n' "${docker_check}" >&2
    fail "Docker Compose is not available." "Install Docker Compose v2, then check docker compose version."
  fi
  if ! docker_check="$(docker info 2>&1)"; then
    printf '%s\n' "${docker_check}" >&2
    fail "Could not connect to the Docker daemon." "Start Docker and check docker info. If it is already running, check your Docker context and socket permissions."
  fi
fi

if [[ "${need_cargo}" == true ]]; then
  require_command cargo
fi

if [[ "${need_node_dependencies}" == true ]]; then
  bootstrap_step="checking npm workspace dependencies"
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
  if [[ ! -x node_modules/.bin/tsx || ! -x node_modules/.bin/vite || ! -x node_modules/.bin/astro || "${stamped_hash}" != "${lock_hash}" ]]; then
    echo "[bootstrap] Installing root npm workspace dependencies..."
    if ! npm install; then
      fail "Could not install npm workspace dependencies." "Check npm's error above for registry, network, or filesystem problems. Fix it and retry npm install from the repository root."
    fi
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
