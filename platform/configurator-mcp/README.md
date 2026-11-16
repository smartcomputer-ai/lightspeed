# Lightspeed Configurator MCP

Streamable HTTP MCP facade over a configurable generated subset of the
universe-scoped Lightspeed JSON-RPC contract. Tools are generated from
`crates/api/contract`; deployment-level `deployment/*` methods can never be
exposed.

The product guide, [Configurator MCP](../../docs/documentation/integrating-and-extending/configurator-mcp.md),
covers the service and its current availability.

The Platform Configurator setup and development-launcher process are disabled
until scoped Platform tokens are available. A runtime token has full authority
over every universe and must never be handed to an agent. The package remains
for development and trusted operator use.

`tool-filter.json` contains the exact universe methods and params omitted from
generation. The default surface excludes the `integration` param of `session/start`,
environment jobs, environment registration keys, and the redundant Lightspeed
handshake.
Edit that file and run `npm run generate` to tune the advertised surface.
Each authenticated request exposes that generated tool set. Method-family
classification is descriptive and grants no restriction on a runtime token.
Tool descriptions come from the canonical Rust method manifest and focus on
operational semantics such as revision guards, lifecycle prerequisites,
idempotency, and secret handling.

## Run

Build the TypeScript client and Configurator, then start the server:

```bash
npm install
npm run build --workspace @lightspeed-ai/configurator-mcp
LIGHTSPEED_AUTH_MODE=single node platform/configurator-mcp/dist/bin.js
```

The default endpoints are:

- MCP: `http://127.0.0.1:18081/mcp`
- health: `http://127.0.0.1:18081/health`
- upstream JSON-RPC: `http://127.0.0.1:18080/rpc`

Configuration:

The central reference is
[environment-variable reference](../../docs/documentation/reference/environment-variables.md#configurator-mcp); the table below
is kept here for service-local convenience.

| Variable | Default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_AUTH_MODE` | `single` | `single` or `authenticated`; must match the upstream gateway |
| `LIGHTSPEED_CONFIGURATOR_MCP_BIND_HOST` | `127.0.0.1` | HTTP bind host |
| `LIGHTSPEED_CONFIGURATOR_MCP_BIND_PORT` | `18081` | HTTP bind port |
| `LIGHTSPEED_CONFIGURATOR_MCP_RPC_URL` | `http://127.0.0.1:18080/rpc` | Lightspeed JSON-RPC endpoint |
| `LIGHTSPEED_CONFIGURATOR_MCP_ALLOWED_HOSTS` | loopback hosts | Comma-separated Host allow-list; required for non-loopback binds |
| `LIGHTSPEED_CONFIGURATOR_MCP_ALLOWED_ORIGINS` | none | Comma-separated browser Origin allow-list |
| `LIGHTSPEED_CONFIGURATOR_MCP_MAX_BODY_BYTES` | `67108864` | Maximum MCP JSON request size |
| `LIGHTSPEED_CONFIGURATOR_MCP_UPSTREAM_TIMEOUT_MS` | `60000` | Per-probe and per-tool upstream timeout |
| `LIGHTSPEED_CONFIGURATOR_MCP_SHUTDOWN_TIMEOUT_MS` | `10000` | Grace period before open HTTP connections are closed |

Authenticated operator requests send `Authorization: Bearer <runtime-token>`
and `x-lightspeed-universe` for tool calls. Configurator verifies the token
with the runtime before handling each MCP request. The optional actor header
supplies attribution only. All configured runtime tokens have full authority;
there are no per-token method groups or universe scopes.

The Platform setup returns unavailable and does not install a credential,
MCP connection, or profile. Do not register this service for an agent with a
runtime token. The replacement will use a restricted token issued by the
Platform.

The server uses the current per-request, sessionless Streamable HTTP lifecycle
and negotiates the 2026 protocol through `server/discover`. It retains the
official SDK's stateless 2025 fallback for older clients. Every request gets a
fresh MCP server instance; no MCP session id or server-side session state is
retained. Current-protocol requests use single JSON responses because the
Configurator does not emit mid-call notifications or subscriptions.

It intentionally has no stdio, legacy SSE endpoint, MCP OAuth, operator tools,
surface profiles, or tool approval overlay.

## Regenerate and verify

```bash
npm run check --workspace @lightspeed-ai/configurator-mcp
```
