# Lightspeed Configurator MCP

Private Streamable HTTP MCP facade over a configurable generated subset of the
universe-scoped Lightspeed JSON-RPC contract. Tools are generated from
`../contract`; deployment-level `operator/*` methods can never be exposed.

`tool-filter.json` contains the exact universe methods omitted from generation.
The default surface excludes provider presence writes, environment jobs,
outbox delivery, and the redundant Lightspeed handshake, leaving 71 tools.
Edit that file and run `npm run generate` to tune the advertised surface.
Tool descriptions come from the canonical Rust method manifest and focus on
operational semantics such as revision guards, lifecycle prerequisites,
idempotency, and secret handling.

## Run

Build the TypeScript client and Configurator, then start the server:

```bash
cd interop/ts-client
npm install
npm run build

cd ../configurator-mcp
npm install
npm run build
LIGHTSPEED_AUTH_MODE=single node dist/bin.js
```

The default endpoints are:

- MCP: `http://127.0.0.1:18081/mcp`
- health: `http://127.0.0.1:18081/health`
- upstream JSON-RPC: `http://127.0.0.1:18080/rpc`

Configuration:

| Variable | Default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_AUTH_MODE` | `single` | `single`, `trusted-header`, or `api-key`; must match the upstream gateway |
| `LIGHTSPEED_CONFIGURATOR_MCP_BIND_HOST` | `127.0.0.1` | HTTP bind host |
| `LIGHTSPEED_CONFIGURATOR_MCP_BIND_PORT` | `18081` | HTTP bind port |
| `LIGHTSPEED_CONFIGURATOR_MCP_RPC_URL` | `http://127.0.0.1:18080/rpc` | Lightspeed JSON-RPC endpoint |
| `LIGHTSPEED_CONFIGURATOR_MCP_ALLOWED_HOSTS` | loopback hosts | Comma-separated Host allow-list; required for non-loopback binds |
| `LIGHTSPEED_CONFIGURATOR_MCP_ALLOWED_ORIGINS` | none | Comma-separated browser Origin allow-list |
| `LIGHTSPEED_CONFIGURATOR_MCP_MAX_BODY_BYTES` | `67108864` | Maximum MCP JSON request size |
| `LIGHTSPEED_CONFIGURATOR_MCP_UPSTREAM_TIMEOUT_MS` | `60000` | Per-probe and per-tool upstream timeout |
| `LIGHTSPEED_CONFIGURATOR_MCP_SHUTDOWN_TIMEOUT_MS` | `10000` | Grace period before open HTTP connections are closed |

In `trusted-header` mode, an authenticating reverse proxy must inject
`x-lightspeed-universe` and may inject `x-lightspeed-principal`. Direct client
access to that listener is unsafe. In `api-key` mode, clients send their
Lightspeed key as `Authorization: Bearer lsk_...`; the Configurator does not
store or resolve it locally.

The first release is stateless JSON-response Streamable HTTP. It intentionally
has no stdio, legacy SSE, MCP OAuth, operator tools, surface profiles, or tool
approval overlay.

## Regenerate and verify

```bash
npm run generate
npm run check
```
