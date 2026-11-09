#!/usr/bin/env bash
# macOS only: install the dependencies for local Lightspeed development.
set -euo pipefail

fail() {
  printf 'setup-deps: %s\n' "$*" >&2
  exit 1
}

case "${1:-}" in
  -h|--help)
    cat <<'EOF'
Usage: ./setup-deps.sh

Install the tools for local Lightspeed development on macOS.
Uses Homebrew for missing packages. Apple, Homebrew, and Docker Desktop
may prompt for first-time setup. After setup completes, run ./dev.sh.
EOF
    exit 0
    ;;
  '') ;;
  *) fail "Unexpected argument. See --help." ;;
esac

[[ "$(uname -s)" == Darwin ]] || fail "This script is for macOS only."
checkout="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$checkout"

if ! xcrun clang --version >/dev/null 2>&1; then
  xcode-select --install || true
  fail "Finish setting up Apple's Command Line Tools, then rerun this script."
fi

find_brew() {
  local bin
  for bin in /opt/homebrew/bin /usr/local/bin; do
    if ! command -v brew >/dev/null && [[ -x "$bin/brew" ]]; then
      export PATH="$bin:$PATH"
    fi
  done
  brew_prefix="$(brew --prefix 2>/dev/null || true)"
}

require_brew() {
  if [[ -z "$brew_prefix" ]]; then
    (
      installer="$(mktemp)"
      trap 'rm -f "$installer"' EXIT
      curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh -o "$installer"
      /bin/bash "$installer"
    )
    find_brew
    [[ -n "$brew_prefix" ]] || fail "Homebrew is unavailable after installation."
  fi
}
find_brew

node_minimum="$(sed -nE 's/^[[:space:]]*"node": ">=([0-9]+\.[0-9]+\.[0-9]+)".*/\1/p' package.json)"
[[ "$node_minimum" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "Could not read the Node.js minimum from package.json."
node_formula="node@${node_minimum%%.*}"

node_ready() {
  command -v npm >/dev/null && node -e '
    const current = process.versions.node.split(".").map(Number);
    const minimum = process.argv[1].split(".").map(Number);
    for (let i = 0; i < minimum.length; i++) {
      if (current[i] !== minimum[i]) process.exit(current[i] < minimum[i] ? 1 : 0);
    }
  ' "$node_minimum" 2>/dev/null
}

node_arch="$(uname -m)"
if [[ "$node_arch" == x86_64 ]]; then
  node_arch=x64
fi
local_node="$checkout/.lightspeed-dev/toolchains/node-v${node_minimum}-darwin-${node_arch}/bin"
if ! node_ready && [[ -x "$local_node/node" ]]; then
  export PATH="$local_node:$PATH"
fi
if ! node_ready; then
  require_brew
  brew_node="$brew_prefix/opt/$node_formula/bin"
  export PATH="$brew_node:$PATH"
  if ! node_ready; then
    if [[ -x "$brew_node/node" ]]; then
      brew upgrade "$node_formula"
    else
      brew install "$node_formula"
    fi
  fi
fi
node_ready || fail "Node.js $node_minimum or newer and npm are required."

if ! command -v rustup >/dev/null && [[ -x "$HOME/.cargo/bin/rustup" ]]; then
  export PATH="$HOME/.cargo/bin:$PATH"
fi
if ! command -v rustup >/dev/null; then
  require_brew
  brew install rustup
  export PATH="$brew_prefix/opt/rustup/bin:$PATH"
fi
export PATH="$(dirname "$(command -v rustup)"):$PATH"
local_rustup="$checkout/.lightspeed-dev/toolchains/rustup"
if [[ -z "${RUSTUP_HOME:-}" && -d "$local_rustup/toolchains" ]]; then
  export RUSTUP_HOME="$local_rustup"
fi
# Run from the checkout so rustup reads the channel and components from rust-toolchain.toml.
rustup toolchain install --no-update --no-self-update

protobuf_ready() {
  [[ -n "$1" && -r "$2/google/protobuf/duration.proto" ]] && "$1" --version >/dev/null 2>&1
}

if [[ -z "${PROTOC:-}${PROTOC_INCLUDE:-}" ]]; then
  brew_protobuf="$brew_prefix/opt/protobuf"
  path_protoc="$(command -v protoc || true)"
  path_include="${path_protoc%/*}/../include"
  if protobuf_ready "$brew_protobuf/bin/protoc" "$brew_protobuf/include"; then
    PROTOC="$brew_protobuf/bin/protoc"
    PROTOC_INCLUDE="$brew_protobuf/include"
  elif protobuf_ready "$path_protoc" "$path_include"; then
    PROTOC="$path_protoc"
    PROTOC_INCLUDE="$path_include"
  else
    require_brew
    brew_protobuf="$brew_prefix/opt/protobuf"
    brew install protobuf
    PROTOC="$brew_protobuf/bin/protoc"
    PROTOC_INCLUDE="$brew_protobuf/include"
  fi
fi
export PROTOC="$(command -v "${PROTOC:-protoc}" || true)"
export PROTOC_INCLUDE="${PROTOC_INCLUDE:-${PROTOC%/*}/../include}"
protobuf_ready "$PROTOC" "$PROTOC_INCLUDE" || fail "Set PROTOC and PROTOC_INCLUDE to a working compiler and its standard .proto files."
"$PROTOC" --version

if ! docker compose version >/dev/null 2>&1; then
  export PATH="$HOME/.docker/bin:/Applications/Docker.app/Contents/Resources/bin:$PATH"
  if ! docker compose version >/dev/null 2>&1 && [[ ! -d /Applications/Docker.app ]]; then
    require_brew
    brew install --cask docker-desktop
  fi
fi
if ! docker info >/dev/null 2>&1; then
  [[ -d /Applications/Docker.app ]] || fail "Start your Docker daemon, then rerun this script."
  printf '%s\n' 'Starting Docker Desktop. Complete any first-run prompts in the app.'
  open -a /Applications/Docker.app
  deadline=$((SECONDS + 60))
  until docker info >/dev/null 2>&1; do
    (( SECONDS < deadline )) || fail "Docker is still starting. Finish Docker Desktop setup, then rerun this script."
    sleep 2
  done
fi
docker compose version || fail "Docker Compose is unavailable. Finish Docker Desktop setup, then rerun this script."

source scripts/dev/lib/npm.sh
ensure_dev_npm_dependencies ci || fail "Could not install the locked npm workspace dependencies."
rust_target="$(rustc -vV | sed -n 's/^host: //p')"
cargo fetch --locked --target "$rust_target"

# Record the selected tools for dev.sh without modifying shell profiles.
mkdir -p .lightspeed-dev
tool_env="$(mktemp .lightspeed-dev/toolchain-env.XXXXXX)"
trap 'rm -f "$tool_env"' EXIT
tool_path="$(dirname "$(command -v node)"):$(dirname "$(command -v npm)"):$(dirname "$(command -v cargo)"):$(dirname "$(command -v docker)")"
{
  printf 'export PATH=%q:"$PATH"\n' "$tool_path"
  if [[ -n "${RUSTUP_HOME:-}" ]]; then
    printf '[[ -n "${RUSTUP_HOME:-}" ]] || export RUSTUP_HOME=%q\n' "$RUSTUP_HOME"
  fi
  printf '[[ -n "${PROTOC:-}" ]] || export PROTOC=%q\n' "$PROTOC"
  printf '[[ -n "${PROTOC_INCLUDE:-}" ]] || export PROTOC_INCLUDE=%q\n' "$PROTOC_INCLUDE"
} > "$tool_env"
mv "$tool_env" .lightspeed-dev/toolchain-env.sh

printf 'Ready: Node.js %s, npm %s, %s\n' "$(node --version)" "$(npm --version)" "$(rustc --version)"
printf '%s\n' 'Start Lightspeed with ./dev.sh'
