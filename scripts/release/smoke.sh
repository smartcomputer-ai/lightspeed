#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

source release/metadata.env
expected_sha="${LIGHTSPEED_GIT_SHA:-$(git rev-parse HEAD)}"
for binary in dist/bin/lightspeed-runtime dist/bin/lightspeed-provider-incus \
  dist/bin/lightspeed-envd dist/bin/lightspeed; do
  version_output="$($binary --version)"
  grep -F "$expected_sha" <<<"$version_output" >/dev/null
done

# The packaged daemon reports the build facts an orchestrator compares with
# the discovery document: this commit, the static target, and the protocol
# number the gateway will demand.
node -e '
  const build = JSON.parse(process.argv[1]);
  const [sha, target, protocol] = process.argv.slice(2);
  if (build.name !== "lightspeed-envd") throw new Error("unexpected build name");
  if (build.gitSha !== sha) throw new Error("envd --print-build git sha mismatch");
  if (build.target !== target) throw new Error(`envd target ${build.target} is not ${target}`);
  if (build.protocolVersion !== Number(protocol)) throw new Error("envd protocol version mismatch");
' "$(dist/bin/lightspeed-envd --print-build)" "$expected_sha" \
  "$LIGHTSPEED_ENVD_TARGET" "$LIGHTSPEED_ENVIRONMENT_PROTOCOL_VERSION"

for archive in dist/archives/*.tar.gz; do
  if [[ "$archive" != *-demo-* && "$archive" != *-docs-* ]]; then
    [[ "$(tar -tzf "$archive" | wc -l)" -eq 1 ]]
  fi
done

demo_tgz="$(find dist/archives -maxdepth 1 -name '*-demo-*' -print -quit)"
test -n "$demo_tgz"
tar -tzf "$demo_tgz" ./index.html >/dev/null
tar -tzf "$demo_tgz" ./favicon.svg >/dev/null
demo_files="$(mktemp)"
trap 'rm -f "$demo_files"' EXIT
tar -tzf "$demo_tgz" > "$demo_files"
grep -Eq '^\./assets/.+\.(css|js)$' "$demo_files"

docs_tgz="$(find dist/archives -maxdepth 1 -name '*-docs-*' -print -quit)"
test -n "$docs_tgz"
for entry in ./index.html ./index.md ./llms.txt ./reference/api.md \
  ./404.html ./pagefind/pagefind.js ./sitemap-index.xml \
  ./assets/ls-logo-2026-v1-ls.svg ./licenses/merriweather-OFL.txt; do
  tar -tzf "$docs_tgz" "$entry" >/dev/null
done
docs_files="$(tar -tzf "$docs_tgz")"
grep -Eq '^\./_astro/.+\.css$' <<<"$docs_files"
grep -Eq '^\./_astro/.+\.woff2$' <<<"$docs_files"
grep -Eq '^\./reference/api/index.html$' <<<"$docs_files"

client_tgz="$(find dist/npm -maxdepth 1 -name '*.tgz' -print -quit)"
for entry in package/package.json package/release.json package/dist/index.js \
  package/schema/api.schema.json; do
  tar -tzf "$client_tgz" "$entry" >/dev/null
done
test -f dist/configurator-mcp/dist/bin.js
test -f dist/configurator-mcp/node_modules/@lightspeed-ai/agent-client/dist/index.js

for runtime in platform platform-workers; do
  test -f "dist/runtime/$runtime.tar.gz"
  tar -tzf "dist/runtime/$runtime.tar.gz" ./package.json >/dev/null
done
tar -tzf dist/runtime/platform.tar.gz ./platform/server/src/main.ts >/dev/null
tar -tzf dist/runtime/platform.tar.gz ./platform/web/dist/index.html >/dev/null
tar -tzf dist/runtime/platform-workers.tar.gz ./platform/connectors/src/host/main.ts >/dev/null
tar -tzf dist/runtime/platform-workers.tar.gz ./platform/connectors/src/providers/telegram/connector.ts >/dev/null
tar -tzf dist/runtime/platform-workers.tar.gz ./platform/connectors/src/providers/whatsapp/connector.ts >/dev/null
platform_worker_files="$(mktemp)"
trap 'rm -f "$demo_files" "$platform_worker_files"' EXIT
tar -tzf dist/runtime/platform-workers.tar.gz > "$platform_worker_files"
grep -Eq '^\./node_modules/baileys/' "$platform_worker_files"

(cd dist && sha256sum --check checksums.txt)
scripts/release/verify-manifest.mjs

# The packaged daemon must reach TLS on its own: a workspace build links two
# rustls providers, and a client config that relied on the process default
# would panic on the first dial. Dial a local self-signed listener twice. On
# the bundled-roots path the daemon must report an untrusted certificate; on
# the operator-CA path it must complete TLS and then be refused the WebSocket
# upgrade by the plain HTTPS responder. Both runs end only when the timeout
# kills a daemon that is still retrying, never by a panic.
probe_dir="$(mktemp -d)"
probe_pid=""
trap 'rm -rf "$demo_files" "$platform_worker_files" "$probe_dir"; if [[ -n "$probe_pid" ]]; then kill "$probe_pid" 2>/dev/null || true; fi' EXIT
mkdir -p "$probe_dir/cwd" "$probe_dir/state"
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
  -keyout "$probe_dir/key.pem" -out "$probe_dir/cert.pem" -days 2 -subj "/CN=localhost" \
  -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" \
  -addext "basicConstraints=critical,CA:FALSE" -addext "extendedKeyUsage=serverAuth" >/dev/null 2>&1
probe_port="$(node -e 'const s=require("node:net").createServer().listen(0,"127.0.0.1",()=>{process.stdout.write(String(s.address().port));s.close();});')"
openssl s_server -accept "127.0.0.1:$probe_port" -cert "$probe_dir/cert.pem" \
  -key "$probe_dir/key.pem" -www >/dev/null 2>&1 &
probe_pid=$!
for _ in $(seq 1 50); do
  if (exec 3<>"/dev/tcp/127.0.0.1/$probe_port") 2>/dev/null; then break; fi
  sleep 0.1
done
envd_bin="$(pwd)/dist/bin/lightspeed-envd"
probe_envd() {
  local log="$1"
  shift
  local status=0
  (cd "$probe_dir" && timeout 8 "$envd_bin" \
    --gateway-url "wss://127.0.0.1:$probe_port/environment-gateway/connect" \
    --cwd "$probe_dir/cwd" --state-dir "$probe_dir/state" "$@") >"$log" 2>&1 || status=$?
  if [[ "$status" -ne 124 ]]; then
    echo "envd exited with status $status instead of retrying until the timeout:" >&2
    cat "$log" >&2
    exit 1
  fi
}
probe_envd "$probe_dir/untrusted.log"
grep -F "invalid peer certificate" "$probe_dir/untrusted.log" >/dev/null
probe_envd "$probe_dir/trusted.log" --ca-file "$probe_dir/cert.pem"
# openssl answers the upgrade request over HTTP/1.0, so the failure the
# trusted run reports comes from the HTTP layer, past a completed TLS
# handshake; any certificate or TLS error there is a real defect.
if grep -Eq "invalid peer certificate|TLS error|CryptoProvider|panicked" "$probe_dir/trusted.log"; then
  echo "envd failed TLS against a trusted listener:" >&2
  cat "$probe_dir/trusted.log" >&2
  exit 1
fi
grep -Eq "WebSocket protocol error|answered HTTP" "$probe_dir/trusted.log"
