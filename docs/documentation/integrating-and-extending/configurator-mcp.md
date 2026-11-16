# Manage Lightspeed through Configurator MCP

The Platform Configurator setup is temporarily unavailable while scoped
Platform tokens are introduced. The development launcher does not start its
process, and the setup installation endpoint refuses requests without creating
a credential, MCP connection, or profile.

A runtime token has full authority over every universe and may name any actor.
It must never be handed to an agent or installed as the credential of an MCP
connection an agent can use. The replacement setup will use a restricted token
issued by the Platform. A Platform browser session cannot currently be exchanged
for such a credential.

The Configurator package remains available for development and trusted operator
use. It translates MCP tool calls into public runtime API requests. This is the
management direction of MCP; an agent using an external server during a task is
covered by [Tools and MCP](../using-lightspeed/tools-and-mcp.md).

<a id="connect-the-three-boundaries"></a>

## Connect the client, Configurator, and runtime

Configurator connects to one configured runtime RPC endpoint. Its authentication
mode must match that runtime:

| Mode | Trusted operator supplies | Runtime connection |
| --- | --- | --- |
| `authenticated` | A configured runtime bearer token and a universe selector for tool calls | Every token reaches every method and universe. An optional actor supplies attribution only. |
| `single` | No identity headers | Private development runtime uses its configured universe. |

Each MCP request authenticates against the runtime before it is handled,
including protocol-only requests. Discovery through `initialize` omits the
universe selector; tool calls carry it. Configurator does not mint tokens or
exchange browser cookies for credentials.

## Run the service for development

Use an artifact from the same release as the runtime. From the repository root:

```bash
npm install
npm run build --workspace @lightspeed-ai/sdk
npm run build --workspace @lightspeed-ai/configurator-mcp
LIGHTSPEED_AUTH_MODE=single node platform/configurator-mcp/dist/bin.js
```

This example requires a runtime explicitly started in `single` mode. The
Configurator listener defaults to `127.0.0.1:18081`, its MCP endpoint to `/mcp`,
and the upstream runtime to `http://127.0.0.1:18080/rpc`. Its `/health` endpoint
returns `ok` when the process is serving; that does not prove upstream access.

The [environment-variable reference](../reference/environment-variables.md#configurator-mcp)
lists the RPC URL, bind address, host and origin allowlists, request-size limit,
and timeouts. Host and origin checks supplement authentication; they grant no
universe access. An authenticated operator uses Streamable HTTP with an
`Authorization: Bearer <runtime-token>` header and `x-lightspeed-universe`.
Do not configure an agent client with that token.

Configurator is stateless and retains no MCP session ID. It has no stdio,
legacy SSE endpoint, MCP OAuth login, or separate approval layer.

## Understand the advertised tool set

Tool names, schemas, and descriptions are generated from the Rust public API.
For example, `lightspeed_profiles_read` maps to `profiles/read`. Descriptions
carry revision guards, lifecycle requirements, and retry semantics.

The [tool filter](../../../platform/configurator-mcp/tool-filter.json) excludes
deployment and service methods, environment jobs, environment registration keys,
the methods that declare and release registered environments, the `integration`
parameter of `session/start`, and the redundant runtime handshake. Change the
filter and regenerate to customize a build; never edit generated tools by hand.

Every authenticated request sees that generated set. Runtime tokens have no
method-group restrictions, and the published API-family classification is
metadata. A generated tool filter does not narrow the authority of the token
itself.

## Inspect before changing resources

Use the discovered input schema. For a revision-guarded update, read the current
resource, preserve the intended fields, and submit the complete replacement
with its expected revision. A session configuration put replaces the sparse
document; omitted features are revoked.

Successful tool results contain one text block holding the public API outcome
as JSON. Its `result` is the method response and `notifications` contains any
returned notifications. A tool result marked `isError: true` contains the
runtime error; MCP transport success does not establish operation success.

Run submission returns after admission, before model completion. Keep a stable
submission ID for retries and retain the returned run ID. A timeout does not
establish whether a mutation was accepted: reconcile current state or retry
with the same identity and contents. See the
[API guide](api-and-typescript.md) for results, cursors, and cancellation.

Configurator forwards bounded event reads as ordinary tool calls. It does not
hold a background session subscription or emit notifications during a tool
call. Keep long-poll intervals within client and Configurator timeouts.

## Verify and diagnose

For trusted operator testing, list tools and read a known resource. Check that
an unconfigured token is rejected. Tokens removed from runtime configuration
stop authenticating after the runtime restarts with that configuration.

| Symptom | What to check |
| --- | --- |
| The Platform setup is unavailable | Expected until restricted Platform tokens are implemented. Do not substitute a runtime token. |
| Health succeeds but tool discovery fails | Upstream reachability, matching authentication mode, and configured token. |
| HTTP 403 before reaching the runtime | Request Host and Origin against the allowlists. |
| A tool call lacks a universe | Send `x-lightspeed-universe` with the target universe. |
| A tool is absent | The generated filter and method scope; deployment and service methods cannot be exposed. |
| A timed-out mutation may have succeeded | Reconcile using its stable IDs and current state before retrying. |

The runtime's internal controller rules still apply to bots and delegated
sessions independently of network caller credentials. See
[Agent and tool access](../access-and-security/agent-and-tool-access.md).
