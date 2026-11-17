# Manage Lightspeed through Configurator MCP

Configurator lets an MCP client configure Lightspeed resources. Use the
Platform setup to give an agent permission to manage profiles, MCP servers,
model defaults, and bots within one universe. For connecting an agent to
other MCP servers, see [Tools and MCP](../using-lightspeed/tools-and-mcp.md).

## Install Configurator for a universe

As an Organization Admin or Platform Admin, open the universe's
**Settings → Templates** and choose **Install** on **Configurator**. Wait
for the setup to show **ready**, then add the `configurator` MCP server to the
profile or session that needs it.

Platform creates a service user with Operator access in that universe and
issues a key limited to a reviewed list of configuration operations there.
The runtime keeps the key in its credential broker, and the installed MCP
connection requires approval for every tool call. Platform checks the
credential on each request. Agent access is limited to the allowed operations
and cannot run sessions or manage credentials or organizations.

Choose **Reinstall and rotate credential** to replace a working credential.
The existing key stays active until its replacement is ready. If the setup
reports an expired or disabled credential, choose **Retry installation**.

<a id="connect-the-three-boundaries"></a>

## Connect the client, Configurator, and runtime

For a Platform deployment, set `LIGHTSPEED_AUTH_MODE=authenticated` on
Configurator and point `LIGHTSPEED_CONFIGURATOR_MCP_API_URL` to Platform's
`/api/v1` URL. Set `LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_URL` on Platform to
the Configurator `/mcp` URL that the runtime can reach. For a private address,
set `LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_ALLOW_PRIVATE_NETWORK=true`. See
[Configurator environment variables](../reference/environment-variables.md#configurator-mcp)
for bind addresses, allowed hosts and origins, and timeouts.

An external MCP client connects to `/mcp` using Streamable HTTP and sends a
Platform API key as `Authorization: Bearer <key>`. Include `initialize` in
the key's operation allowlist so Configurator can verify it through API
discovery. A key scoped to one accessible universe gets that universe as its
default. Otherwise, supply its UUID in the MCP client's
`x-lightspeed-universe` header.

An infrastructure operator can instead point a standalone Configurator at
a private runtime and authenticate with a runtime token and universe UUID.
Keep this connection for trusted operators. Managed agent setups use Platform
keys, which carry the checks on universe and operation permissions.

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
`lightspeed_profiles_read` calls `profiles/read`. The installed agent connection
limits the listed tools to a configuration allowlist. An external client can
see the generated tool set, with access enforced by its key and role on each
call. See the [Configurator package guide](../../../platform/configurator-mcp/README.md)
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
| The managed setup is unavailable | Set the Platform Configurator URL and confirm that the administrator can install in the universe. |
| Health succeeds but discovery fails | Check upstream reachability, authentication mode, and whether the key permits `initialize`. |
| A tool call lacks a universe | Scope the key to one accessible universe or supply its UUID in `x-lightspeed-universe`. |
| A tool appears but the call is forbidden | Check the key restrictions, the identity's role, and the universe's enabled features. |
