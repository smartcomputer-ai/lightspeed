# Lightspeed Configurator MCP

Streamable HTTP MCP facade over a configurable generated subset of the
universe-scoped Lightspeed HTTP contract. Tools are generated from
`crates/api/contract`; deployment-level `deployment/*` methods can never be
exposed.

The product guide, [Configurator MCP](../../docs/documentation/integrating-and-extending/configurator-mcp.md),
covers the service and its current availability.

The Platform setup installs a universe-scoped service-user key and a managed
MCP server connection. `./dev.sh platform` and `./dev.sh full` start the
facade with Platform as its upstream. Keep direct runtime credentials for
trusted infrastructure: a runtime token has full deployment authority.

`tool-filter.json` contains the exact universe methods and params omitted from
generation. The default surface excludes the `integration` param of `session/start`,
environment jobs, environment registration keys, and the redundant Lightspeed
connection probe and binary upload operation.
Edit that file and run `npm run generate` to tune the advertised surface.
External clients see the generated tool set. The managed Platform setup
restricts the tools an agent sees separately from the operations its key can
call.
Tool descriptions come from the canonical Rust operation catalog and focus on
operational semantics such as revision guards, lifecycle prerequisites,
idempotency, and secret handling.

## Run

Build the TypeScript client and Configurator, then start the server:

```bash
npm install
npm run build --workspace @lightspeed-ai/sdk
npm run build --workspace @lightspeed-ai/configurator-mcp
LIGHTSPEED_AUTH_MODE=single node platform/configurator-mcp/dist/bin.js
```

The default endpoints are:

- MCP: `http://127.0.0.1:18081/mcp`
- health: `http://127.0.0.1:18081/health`
- upstream HTTP API: `http://127.0.0.1:18080/api/v1`

Configuration:

The central reference is
[environment-variable reference](../../docs/documentation/reference/environment-variables.md#configurator-mcp); the table below
is kept here for service-local convenience.

| Variable | Default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_AUTH_MODE` | `single` | `authenticated` for Platform or an authenticated runtime, `single` for a private single-mode runtime |
| `LIGHTSPEED_CONFIGURATOR_MCP_BIND_HOST` | `127.0.0.1` | HTTP bind host |
| `LIGHTSPEED_CONFIGURATOR_MCP_BIND_PORT` | `18081` | HTTP bind port |
| `LIGHTSPEED_CONFIGURATOR_MCP_API_URL` | `http://127.0.0.1:18080/api/v1` | Lightspeed HTTP API base |
| `LIGHTSPEED_CONFIGURATOR_MCP_ALLOWED_HOSTS` | loopback hosts | Comma-separated Host allow-list; required for non-loopback binds |
| `LIGHTSPEED_CONFIGURATOR_MCP_ALLOWED_ORIGINS` | none | Comma-separated browser Origin allow-list |
| `LIGHTSPEED_CONFIGURATOR_MCP_MAX_BODY_BYTES` | `67108864` | Maximum MCP JSON request size |
| `LIGHTSPEED_CONFIGURATOR_MCP_UPSTREAM_TIMEOUT_MS` | `60000` | Per-probe and per-tool upstream timeout |
| `LIGHTSPEED_CONFIGURATOR_MCP_SHUTDOWN_TIMEOUT_MS` | `10000` | Grace period before open HTTP connections are closed |

For managed use, configure the Platform `/api/v1` URL as the upstream and
set `LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_URL` on Platform to the facade's
`/mcp` URL. In the universe's **Settings → Templates**, an Organization Admin
or Platform Admin can install **Configurator** and rotate its key.

Authenticated requests send the upstream credential as a bearer token.
Configurator verifies it through the upstream's `/connection` endpoint before
handling each MCP request. Include `initialize` in any Platform key operation
allowlist. Send the universe UUID as `x-lightspeed-universe` for tool calls
when discovery hasn't returned a default. Configurator uses this UUID in
the upstream resource path.

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
