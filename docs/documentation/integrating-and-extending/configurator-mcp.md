# Manage Lightspeed through Configurator MCP

Configurator exposes Lightspeed management operations as MCP tools. An MCP
client can inspect profiles, create sessions, read runs, and manage other
resources in its authorized universe. The service translates each tool call
into the corresponding public API request; the runtime remains responsible
for the operation and its durable state.

This is the management direction of MCP. An agent using an external MCP
server during a task is covered by [Tools and MCP](../using-lightspeed/tools-and-mcp.md).
Both paths can exist in one installation, including a Lightspeed agent using
Configurator to manage resources in its universe.

<a id="connect-the-three-boundaries"></a>

## Connect the client, Configurator, and runtime

A Configurator integration connects the MCP client to the Configurator
service, then Configurator to a runtime gateway. Its authentication mode must
match that gateway:

| Mode | MCP client or trusted upstream supplies | Runtime connection |
| --- | --- | --- |
| `authenticated` | A bearer key; deployment keys also select a universe | Runtime validates the key, scope, method group, and any actor assertion. |
| `single` | No identity headers | Private development gateway uses its configured universe. |

A Platform browser login is not a Configurator credential. Configurator forwards
credentials to the runtime; it does not exchange cookies for keys. An actor
header supplies attribution only, and requires a key allowed to assert actors.
A universe key must omit the universe selector; a deployment key must send it.
See [API keys and service access](../access-and-security/api-keys-and-service-access.md).

## Run the service

Use a Configurator artifact from the same release as the runtime. For source
development, build it from the repository root with Node.js 24 or newer:

```bash
npm install
npm run build --workspace @lightspeed-ai/agent-client
npm run build --workspace @lightspeed/configurator-mcp
```

For a local Configurator connected to your existing API-key gateway, set
`LIGHTSPEED_API_URL` to that gateway's `/rpc` endpoint, then run:

```bash
LIGHTSPEED_AUTH_MODE=authenticated \
LIGHTSPEED_CONFIGURATOR_MCP_RPC_URL="$LIGHTSPEED_API_URL" \
node platform/configurator-mcp/dist/bin.js
```

The default listener is `127.0.0.1:18081`. Its MCP endpoint is `/mcp` and its
liveness endpoint is `/health`:

```bash
curl --fail http://127.0.0.1:18081/health
```

The health response is `ok`. Next, list tools and read a resource as shown
below to check the client's key and the connection to the runtime.

For a deployed listener, configure a bind address and host allowlist,
for example:

```dotenv
LIGHTSPEED_AUTH_MODE=authenticated
LIGHTSPEED_CONFIGURATOR_MCP_BIND_HOST=0.0.0.0
LIGHTSPEED_CONFIGURATOR_MCP_BIND_PORT=18081
LIGHTSPEED_CONFIGURATOR_MCP_RPC_URL=http://lightspeed-api-gateway:18080/rpc
LIGHTSPEED_CONFIGURATOR_MCP_ALLOWED_HOSTS=configurator.example.com
```

Place the service behind your HTTPS edge and route its `/mcp` path to this
listener. Preserve the public host expected by the allowlist and forward the
client's authorization header. If the proxy sends another upstream host,
add that host to the allowlist. Host entries are hostnames, without
schemes or ports.

Set `LIGHTSPEED_CONFIGURATOR_MCP_ALLOWED_ORIGINS` only for the browser origins
that should call it. Requests without an `Origin` header are accepted by that
check; requests with one must match the configured list. Host and origin
checks supplement authentication. They do not grant access to a universe.

The [variable reference](../reference/environment-variables.md#configurator-mcp)
contains body-size, upstream-timeout, and shutdown settings. Configurator
receives client keys per request; it does not need one shared client key in
its process environment.

## Configure an MCP client

Use the client's Streamable HTTP or remote HTTP server configuration:

| Field | Value |
| --- | --- |
| URL | `https://configurator.example.com/mcp`, or the local `/mcp` URL above |
| Transport | Streamable HTTP |
| Authorization | The universe's Lightspeed bearer key, stored in the client's secret/header configuration |
| Tools | Discover with the client's MCP tool-list operation |

Enter these settings in your client's configuration format. It must support
remote HTTP with a bearer header; Configurator provides no stdio, legacy SSE,
or MCP OAuth login flow. Let the client negotiate the protocol. Configurator
is stateless and does not retain an MCP session ID between requests.

## Inspect before changing resources

Begin by discovering the tools and inspecting the release-editor profile.
The MCP `tools/call` parameters are:

```json
{
  "name": "lightspeed_profiles_read",
  "arguments": { "profileId": "release-editor" }
}
```

Successful results contain one text block holding JSON. Parse that JSON as
the public API outcome: its `result` holds the method response, and
`notifications` holds notifications returned with it. Configurator does not
also return a duplicate `structuredContent` representation.

Use the discovered input schema for the next operation. For a revision-guarded
update, first read the current resource, preserve fields you intend to keep,
and submit the complete replacement with its expected revision. A session
configuration put replaces the sparse document; omitted features are revoked.
An agent should not reconstruct a replacement from an old conversational
summary when it can read the current document.

For the Acorn example, a useful client instruction is:

> Read the release-editor profile and explain its model, workspace access, and
> instructions. Then list the sessions using that setup so I can choose the
> one to inspect.

The client performs this sequence with the discovered tools and the permissions
of its key. If a list method cannot filter by profile, it can inspect the
returned records using the supported schema. The example needs both `profiles`
and `session` access.

## Submit work and follow its result

The Configurator tool for starting a run has the same semantics as
`session/runs/start`: it returns after admission, not after model completion.
Provide a stable submission ID for retries, retain the returned run ID, and
follow the session event stream or read that run until terminal.

For example, these `tools/call` parameters submit to an existing session:

```json
{
  "name": "lightspeed_session_runs_start",
  "arguments": {
    "sessionId": "acorn-release-review",
    "submissionId": "acorn-1.2-review-001",
    "source": {
      "type": "input",
      "items": [
        { "type": "text", "text": "Review the Acorn 1.2 release notes and report any unsupported claims." }
      ]
    }
  }
}
```

Use a key with `session` access, an actual session ID, and a configured profile.
A timeout at
the MCP client or Configurator does not establish whether the runtime accepted
the operation. Retry with the same submission ID and contents or reconcile
the known run. The [API guide](api-and-typescript.md) explains admission,
cursors, results, and cancellation in detail.

Configurator forwards bounded event-read calls as ordinary tool results. It
does not maintain a background session subscription or stream notifications
while another tool call executes. Keep an event read's long-poll interval
within the Configurator and client's request timeouts.

## Understand the advertised tool set

Tool names and schemas are generated from the Rust public API. Names such as
`lightspeed_profiles_read` map to methods such as `profiles/read`. The generated
descriptions carry operational details, including revision guards, lifecycle
requirements, and retry behavior.

Only the configured subset of ordinary universe methods is exposed. Deployment
and service methods are excluded. The default filter also omits managed-session
creation, environment-job methods, environment-registration-key methods, and
the redundant runtime handshake. Configurator is therefore not a deployment
administration interface or a replacement for the workflow integration client.

The repository's [tool filter](../../../platform/configurator-mcp/tool-filter.json)
controls generation exclusions. To change that surface in a custom build,
edit the filter and regenerate; do not edit
`src/generated/tools.ts`. A filter is deployment-wide, not a per-user
authorization policy.

Within that surface, each request sees only the tools its key may call. The
Configurator reads the key's method groups from the runtime's handshake on
every request, lists only tools in those groups, and treats any other tool as
unknown. A key minted with the configuration groups therefore sees no session
or workspace tools. The runtime still checks every call. Configurator has no
separate tool-approval layer; use the calling client's controls and the key's
method groups.

## Let a Lightspeed agent use Configurator

The Platform's Configurator setup requires a configured
`LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_URL`. That setting points to an existing
service; it does not launch Configurator. The setup registers the MCP
connection in the universe. The consuming profile still needs the applicable
MCP capability and server selection.

The setup creates a universe key and stores it in an outbound credential grant.
Its groups are `profiles`, `mcp`, `environments`, `bots`, `channels`, `auth`, and
`models`; it cannot assert actors or call session or VFS methods. Both
Configurator and runtime use authenticated mode, including in full development.

Granting Configurator to an agent grants management operations in its universe,
which can include modifying resources used by other sessions. The caller's
Platform role and private-session visibility do not constrain those tool calls:
they use the stored key. Choose the calling profile and approval controls with
those effects in mind. See [Agent and tool access](../access-and-security/agent-and-tool-access.md)
for this boundary and the separate limits on internal controllers.

## Verify and diagnose

List tools, read a known profile, and verify a small operation in a disposable
session using a key with `session` access. Then test rejection with a revoked
test key. Configurator validates the key upstream on each MCP request, including
protocol-only requests, so a successful earlier connection does not preserve
access after revocation.

| Symptom | What to check |
| --- | --- |
| Health succeeds but tool discovery fails | Upstream reachability, matching gateway mode, key status, and the universe header required by the key's scope. |
| HTTP 403 before reaching the runtime | The request's Host and Origin against the allowlists. |
| The client expects an SSE stream or session ID | Select Streamable HTTP with a supported negotiated protocol; Configurator is sessionless. |
| A tool is absent | Check the generated filter and method scope. Deployment/service methods cannot be exposed. |
| A call returns `isError: true` | Parse the JSON error text for the runtime error kind and details; an MCP transport success does not imply an API operation succeeded. |
| A timed-out mutation may have succeeded | Reconcile using the operation's stable IDs and current resource state before retrying. |
