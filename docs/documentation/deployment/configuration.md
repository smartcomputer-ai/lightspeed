# Configure a deployment

A Lightspeed installation has several processes, each with its own
configuration. The runtime owns agent execution and storage; the Platform
owns login and management; optional services provide compute, chat bridges,
and Configurator MCP. Start by deciding which process consumes a setting and
which other processes must agree with it.

This guide explains those choices. The
[environment-variable reference](../reference/environment-variables.md)
contains the complete names, defaults, and requirements. Use the
[self-hosting recipe](self-hosting.md) for a minimal full installation, then
add the groups below as the deployment needs them.

## Supply settings to the right process

Keep separate configuration for runtime, Platform, and optional services.
For example, `LIGHTSPEED_POSTGRES_URL` selects the runtime database, while
`LIGHTSPEED_PLATFORM_DATABASE_URL` selects the Platform database. Giving one
process the other's URL does not connect the two applications correctly; their
schemas and migration histories are independent.

`LIGHTSPEED_API_URL` is a client setting used by the Platform, CLI, and
connector host. It does not configure where the runtime listens. That address
comes from `LIGHTSPEED_GATEWAY_BIND`.

In deployed containers, supply explicit environment variables or protected
environment files. Docker `--env-file` uses `KEY=value`, without `export` or
shell quotes. Shell startup scripts use shell syntax instead. A file that
works with `source` is not necessarily interpreted the same way by Docker.

The Rust server, CLI, evaluator, and Incus provider can load a `.env` file from
the working directory or its parents. Avoid an unrelated checkout's `.env`
becoming implicit production configuration. The development launcher applies
additional local defaults, including public development credentials. Use its
settings for local work only.

Environment settings are read by the processes that use them. Recreate or
restart the affected service after changing its environment. A provider
integration edited through the Platform is a stored universe resource and
follows its own update path; it is not a deployment environment-file edit.

## Keep deployment-wide settings aligned

All runtime roles serving one deployment need a coherent view of its durable
state and routing:

| Setting group | What must agree |
| --- | --- |
| Runtime storage | Runtime PostgreSQL database and selected blob backend, including object bucket/prefix where configured. |
| Temporal | Frontend address, namespace, and the session, bot, and channel task-queue names. |
| Secrets | The master key used to encrypt and resolve stored credentials. |
| Environment routing | The one environment gateway's internal URL and shared routing token. |
| Provider transport | Required deployment fallbacks and network access for the roles performing discovery or generation. |

Roles and listener addresses can differ by process. Authenticated gateways
serve both Platform service credentials and direct clients, each constrained by
canonical principal rights and credential scope. See
[Authentication and access](authentication-and-tenancy.md).

If multiple deployments share a Temporal namespace, set all three queue
variables independently. A deployment-specific `LIGHTSPEED_TASK_QUEUE` does
not change `LIGHTSPEED_TASK_QUEUE_BOTS` or
`LIGHTSPEED_TASK_QUEUE_CHANNELS`. Keep the connector host in the matching
Temporal namespace too.

## Distinguish public URLs from internal routes

Several addresses can refer to the same installation while serving different
callers:

| Setting | Example | Caller and purpose |
| --- | --- | --- |
| `LIGHTSPEED_GATEWAY_BIND` | `0.0.0.0:18080` | Address inside the runtime's network namespace where HTTP listens. |
| `LIGHTSPEED_API_URL` on Platform | `http://lightspeed-runtime:18080/rpc` | Private Platform-to-runtime JSON-RPC route. |
| `LIGHTSPEED_PLATFORM_BASE_URL` | `https://lightspeed.example.com` | Public browser origin used for Platform authentication. |
| `LIGHTSPEED_PUBLIC_BASE_URL` | `https://lightspeed.example.com` | Public runtime origin used for OAuth callbacks, bot hooks, and environment addresses. |
| `LIGHTSPEED_ENVIRONMENT_GATEWAY_URL` | `http://lightspeed-runtime:18080` | Private base URL workers use for environment routes; omit `/rpc`. |
| `LIGHTSPEED_ENVIRONMENT_PUBLIC_URL` | `https://compute.example.com` | Optional separate public base for daemon data connections. |

An address containing `localhost` is relative to the process using it. Inside
a container it refers to that container. Conversely, binding to `0.0.0.0`
does not make that a useful public callback URL. Set the external origins
explicitly behind a reverse proxy.

Preserve paths, query strings, host, and scheme as described in
[Configure the public edge](self-hosting.md#configure-the-public-edge).
Daemon control and data routes require WebSocket upgrades and long-lived
connections. Workers also need the private environment route even when the
public WebSockets are reachable.

The internal environment token is separate from a user's API key or a daemon
registration key. Set the same `LIGHTSPEED_ENVIRONMENT_GATEWAY_TOKEN` on the
environment gateway and the processes calling it. A process without the
`environment-gateway` role requires both the URL and token, including a
gateway-only process.

## Preserve the keys that protect durable state

Generate `LIGHTSPEED_SECRETS_MASTER_KEY` once for the deployment:

```bash
openssl rand -base64 32
```

Store the output in the deployment's secret manager and recovery material.
It must decode to 32 bytes. All runtime processes resolving the same stored
secrets must use that key. Replacing it is not an automatic key rotation: the
existing ciphertext remains encrypted with the previous value.

Keep the Platform authentication secret stable too. If WhatsApp is enabled,
preserve its media-locator key and authentication directory. These values
protect different state and are not interchangeable. The
[recovery inventory](upgrades-and-recovery.md#preserve-a-complete-recovery-set)
explains what needs to be retained together.

Provider API keys can be stored per universe through **Settings →
Integrations**. This keeps the credential associated with the team that uses
it. Deployment-level `OPENAI_API_KEY` and `ANTHROPIC_API_KEY` are fallback
credentials when the corresponding universe record is absent. A disabled or
broken stored record blocks fallback.

`LIGHTSPEED_CHAT_PROVIDER` and `LIGHTSPEED_CHAT_MODEL` change the runtime's
default provider ID and model name. Its default API kind remains
`openai:responses`; changing a provider ID alone does not switch wire formats.
For Anthropic or another route, select provider, API kind, and model explicitly
in a profile. See [Models and credentials](../using-lightspeed/models-and-credentials.md).

## Choose the blob backend

With all `LIGHTSPEED_OBJECT_STORE_*` variables absent, blobs up to 64 KiB are
stored inline in PostgreSQL. Larger writes fail. This supports an initial
small-text walkthrough; configure S3-compatible storage before working with
larger files, attachments, or other payloads.

To use S3-compatible storage for blobs, configure the full group. For example:

```dotenv
LIGHTSPEED_OBJECT_STORE_BUCKET=lightspeed-content
LIGHTSPEED_OBJECT_STORE_REGION=us-east-1
LIGHTSPEED_OBJECT_STORE_PREFIX=production
```

Supply the credentials required by the object-store client. Add
`LIGHTSPEED_OBJECT_STORE_ENDPOINT` for a custom service and
`LIGHTSPEED_OBJECT_STORE_FORCE_PATH_STYLE=true` when that service requires
path-style addressing. The [reference](../reference/environment-variables.md#object-storage)
lists the exact settings.

Setting any variable in this group activates its configuration requirements,
even if the value is empty. An endpoint or prefix without a nonempty bucket
causes startup to fail. Remove the entire group when intentionally using only
PostgreSQL.

With object storage configured, small blobs still remain inline in PostgreSQL;
larger blobs go to the object store. PostgreSQL keeps their catalog and exact
physical object keys. A prefix change affects new keys, while existing records
continue to reference their original keys. Keep those objects accessible.
Changing a bucket or endpoint does not copy existing content, so plan that
change as a data migration and test old attachments and workspace files as
well as newly written content.

`LIGHTSPEED_BLOB_CACHE_BYTES` controls a per-process cache. Increasing worker
replicas also increases the aggregate cache budget. Blob collection is a
separate policy, described in [operations](operations.md#manage-retention-and-blob-collection).

## Add optional services

### Execution environments

The daemon receives its own `LIGHTSPEED_ENVD_*` settings on the machine where
it runs. Its working directory and filesystem root describe that machine,
not the runtime's VFS. Retain its state directory when its registered identity
should survive restarts.

The Incus provider uses a JSON configuration file selected by `--config` or
`LIGHTSPEED_INCUS_PROVIDER_CONFIG`. Templates, Incus credentials, and ingress
policy belong there. The Platform then registers the provider and binds it
to universes. Follow [Bring your own compute](../environments/bring-your-own-compute.md)
or [Incus VMs](../environments/incus-vms.md) for complete procedures.

### Chat connectors

The connector host uses `LIGHTSPEED_CONNECTOR_API_KEY` against an authenticated
runtime and the deployment's Temporal namespace. It needs scoped service
capabilities for discovery, leasing and inbound admission. Provider bot tokens
are leased from core rather than configured on the connector host.

WhatsApp additionally needs `LIGHTSPEED_CONNECTOR_WHATSAPP_AUTH_DIR` on
persistent storage and a stable
`LIGHTSPEED_CONNECTOR_WHATSAPP_MEDIA_LOCATOR_KEY`. Configure
`LIGHTSPEED_PLATFORM_CHANNELS_HEALTH_URLS` with the internal connector health
base URLs so Platform administrators can see host status. This aggregation
does not start connector workers.

The default host discovers all enabled accounts for its selected providers.
If running several hosts, give them nonoverlapping account selections. See
[Chat channels](../using-lightspeed/chat-channels.md) for setup and
[operations](operations.md#scale-the-process-that-does-the-work) for scaling.

### MCP and Configurator

An agent's native MCP connection is a stored universe resource. To reach a
private endpoint, both the server record's `allowPrivateNetwork` and the
deployment's `LIGHTSPEED_MCP_PRIVATE_NETWORKS` must permit it. OAuth metadata
and token requests have a separate private-network setting. Permitting OAuth
traffic does not authorize tool discovery or execution.

Configurator is a separately deployed MCP server that manages Lightspeed. Its
mode must match its upstream gateway, and a non-loopback listener requires an
explicit allowed-host configuration. The Platform's Configurator URL enables
its setup flow; it does not start the service. The setup provisions a dedicated
universe service identity and key. The old credentialless path is retired.

See [Tools and MCP](../using-lightspeed/tools-and-mcp.md) and the
[Configurator variables](../reference/environment-variables.md#configurator-mcp)
before enabling those paths.

## Verify a configuration change

First confirm the affected process starts with the intended settings. Then
exercise the boundary that changed: read existing content after a storage
change, complete a run after a queue or provider change, or perform a harmless
machine operation after an environment-route change. A successful listener
health check alone cannot establish those results.

| Symptom | What to check |
| --- | --- |
| A changed variable has no effect | Confirm which process reads it and recreate that process with the new environment. |
| A split runtime process fails at startup | Supply the environment gateway URL/token and the same stores, key, namespace, and queue settings. |
| An object-store setting causes a bucket error | Supply the complete group or remove every `LIGHTSPEED_OBJECT_STORE_*` variable. |
| New files work but old content is missing | Check the old physical blob location; configuration changes do not migrate bytes. |
| Credentials cannot be decrypted after restart | Restore the matching master key and universe UUID mapping. |
| OAuth returns to an internal hostname | Set the public base URL and verify the proxy preserves the intended public origin. |
