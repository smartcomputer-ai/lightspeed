# Manage Lightspeed through Configurator MCP

The Platform Configurator setup installs a configuration-only MCP connection.
An Organization Admin who owns the universe installs it from **Settings → Templates**.
Platform creates an organization service user with an explicit Operator grant in that
universe and a restricted API key. The key permits only the reviewed
configuration operations, cannot create universes, and cannot manage organizations,
credentials, or sessions.

The runtime stores that key as a brokered credential and uses it for the
`configurator` MCP server. Tool calls go from the runtime to Configurator, then
to Platform's shared API. Platform checks the service user and key on each
request before forwarding an admitted call to the runtime. The MCP connection
also limits advertised tools to the configuration allowlist and requires
approval. No human session is synthesized, and no runtime infrastructure token
is passed to Configurator or the agent.

The installation creates an MCP connection, not a profile. Add that server to
a profile or session configuration as appropriate. Reinstalling rotates the
managed service-user credential, retaining the previous credential until the
replacement is ready. A failed replacement keeps the working installation. Expired keys or disabled service users are
reported as unavailable and need reinstallation.

For agents using other external MCP servers, see
[Tools and MCP](../using-lightspeed/tools-and-mcp.md).

<a id="connect-the-three-boundaries"></a>

## Connect the client, Configurator, and runtime

Configurator connects to one configured HTTP API base URL. For managed agent
use, configure the **Platform** `/api/v1` endpoint and authenticated mode.
Every request authenticates through discovery first, including MCP protocol
requests. A key restricted to exactly one accessible universe receives that
UUID as `defaultUniverse`; Configurator uses it for resource calls.

For trusted operator use, Configurator can instead address a direct runtime
with a runtime token and an explicit universe. That token grants full runtime
authority and must never be installed for an agent. `single` mode is available
only for private development against a matching unauthenticated runtime.
Configurator does not mint tokens or exchange browser cookies.

## Run the service for development

The supported `./dev.sh platform` and `./dev.sh full` profiles start Configurator
on `127.0.0.1:18081`, pointing to Platform at port 3000 by default. They give
Configurator an explicit environment without database, auth-server, or runtime
infrastructure secrets.

For standalone single-mode development, use an artifact from the same release
as the runtime. From the repository root:

```bash
npm install
npm run build --workspace @lightspeed-ai/sdk
npm run build --workspace @lightspeed-ai/configurator-mcp
LIGHTSPEED_AUTH_MODE=single node platform/configurator-mcp/dist/bin.js
```

This example requires a runtime explicitly started in `single` mode. The
Configurator listener defaults to `127.0.0.1:18081`, its MCP endpoint to `/mcp`,
and the upstream runtime to `http://127.0.0.1:18080/api/v1`. Its `/health` endpoint
returns `ok` when the process is serving; that does not prove upstream access.

The [environment-variable reference](../reference/environment-variables.md#configurator-mcp)
lists the API URL, bind address, host and origin allowlists, request-size limit,
and timeouts. Host and origin checks supplement authentication; they grant no
universe access. An authenticated operator uses Streamable HTTP with an
`Authorization: Bearer <runtime-token>` header and `x-lightspeed-universe`.
The latter selects Configurator's request context; Configurator puts that
universe in the upstream resource URL rather than forwarding the header.
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

The managed setup narrows advertised tools and API operations to its explicit
configuration allowlist. Standalone trusted operator clients see the generated
set. Runtime tokens have no method-group restrictions, and the API-family classification is
metadata. A generated tool filter does not narrow the authority of the token
itself.

## Inspect before changing resources

Use the discovered input schema. For a revision-guarded update, read the current
resource, preserve the intended fields, and submit the complete replacement
with its expected revision. A session configuration put replaces the sparse
document; omitted features are revoked.

Successful tool results contain one text block holding the public API response
as JSON. Binary reads return a JSON object with `encoding: "base64"`, `data`,
and `bytes`. A tool result marked `isError: true` contains the
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
| The Platform setup is unavailable | Configure `LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_URL`, install as an Organization Admin, and check the service-user/key status. Reinstall to replace expired credentials. |
| Health succeeds but tool discovery fails | Upstream reachability, matching authentication mode, and configured token. |
| HTTP 403 before reaching the runtime | Request Host and Origin against the allowlists. |
| A tool call lacks a universe | Send `x-lightspeed-universe` with the target universe. |
| A tool is absent | The generated filter and method scope; deployment and service methods cannot be exposed. |
| A timed-out mutation may have succeeded | Reconcile using its stable IDs and current state before retrying. |

The runtime's internal controller rules still apply to bots and delegated
sessions independently of network caller credentials. See
[Agent and tool access](../access-and-security/agent-and-tool-access.md).
