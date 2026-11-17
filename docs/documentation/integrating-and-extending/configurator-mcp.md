# Manage Lightspeed through Configurator MCP

Configurator lets an MCP client, such as a coding agent on your machine,
configure Lightspeed resources: profiles, MCP servers, model defaults,
environments and bots. It is a separate service that you run and connect with
a Platform API key. For connecting an agent to other MCP servers, see
[Tools and MCP](../using-lightspeed/tools-and-mcp.md).

<a id="connect-the-three-boundaries"></a>

## Connect the client, Configurator, and runtime

For a Platform deployment, set `LIGHTSPEED_AUTH_MODE=authenticated` on
Configurator and point `LIGHTSPEED_CONFIGURATOR_MCP_API_URL` to Platform's
`/api/v1` URL. See
[Configurator environment variables](../reference/environment-variables.md#configurator-mcp)
for bind addresses, allowed hosts and origins, and timeouts.

An external MCP client connects to `/mcp` using Streamable HTTP and sends a
Platform API key as `Authorization: Bearer <key>`. Include `initialize` in
the key's operation allowlist so Configurator can verify it through API
discovery. Restrict the key to the universe and the operations the client
needs; the key's restrictions and its holder's role decide what each call may
do. A key scoped to one accessible universe gets that universe as its
default. Otherwise, supply its UUID in the MCP client's
`x-lightspeed-universe` header.

An infrastructure operator can instead point a standalone Configurator at
a private runtime and authenticate with a runtime token and universe UUID.
Keep this connection for trusted operators: a runtime token has full
deployment authority. Platform keys carry the checks on universe and
operation permissions.

## Run the service for development

The `./dev.sh platform` and `./dev.sh full` profiles start an authenticated
Configurator with Platform as its upstream. Its default URL is
`http://127.0.0.1:18081/mcp`.

For standalone development against a private single-mode runtime, build and
run Configurator in the same mode.

```bash
npm install
npm run build --workspace @lightspeed-ai/sdk
npm run build --workspace @lightspeed-ai/configurator-mcp
LIGHTSPEED_AUTH_MODE=single node platform/configurator-mcp/dist/bin.js
```

This uses `http://127.0.0.1:18080/api/v1` as the default upstream and exposes
`/mcp` and `/health` on port 18081. Check a tool call against an existing
resource to verify upstream access.

## Understand the advertised tool set

Tool names and input schemas come from the resource API. For example,
`lightspeed_profiles_read` calls `profiles/read`. A client sees the
generated tool set, with access enforced by its key and role on each call. See the [Configurator package guide](../../../platform/configurator-mcp/README.md)
to customize the generated tool set.

## Inspect before changing resources

For a revision-guarded update, read the resource first and submit the intended
replacement with its expected revision. Tool results contain the API response
as JSON text. Check `isError` for operation failures. If a mutation times out,
read current state before retrying. For run submission and event cursors, see
[Build with the API and TypeScript client](api-and-typescript.md).

## Verify and diagnose

| Symptom | What to check |
| --- | --- |
| Health succeeds but discovery fails | Check upstream reachability, authentication mode, and whether the key permits `initialize`. |
| A tool call lacks a universe | Scope the key to one accessible universe or supply its UUID in `x-lightspeed-universe`. |
| A tool appears but the call is forbidden | Check the key restrictions, the identity's role, and the universe's enabled features. |
