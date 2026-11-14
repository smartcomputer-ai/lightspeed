# Configure a deployment

A Lightspeed installation has several processes, each with its own
configuration. The runtime owns agent execution and storage; the Platform
owns login and management; optional services provide compute, chat bridges,
and Configurator MCP. Put each setting on the process that uses it, and keep
shared settings consistent across replicas.

This guide explains those choices. The
[environment-variable reference](../reference/environment-variables.md)
contains the complete names, defaults, and requirements. Use the
[self-hosting recipe](self-hosting.md) for a minimal full installation, then
add the optional services below as the deployment needs them.

## Supply settings to the right process

Keep separate configuration for runtime, Platform, and optional services.
For example, `LIGHTSPEED_POSTGRES_URL` selects the runtime database, while
`LIGHTSPEED_PLATFORM_DATABASE_URL` selects the Platform database. Giving one
process the other's URL does not connect the two applications correctly; their
schemas and migration histories are independent.

`LIGHTSPEED_API_URL` is a client setting used by the Platform, CLI, and
connector host. It does not configure where the runtime listens. That address
comes from `LIGHTSPEED_GATEWAY_BIND`.

In deployed containers, supply environment variables or protected
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
| Temporal | Frontend address, namespace, TLS and credential settings, and the session, bot, and channel task-queue names. |
| Secrets | The master key used to encrypt and resolve stored credentials. |
| Environment routing | The one environment gateway's internal URL and shared routing token. |
| Provider transport | Required deployment fallbacks and network access for the roles performing discovery or generation. |

Roles and listener addresses can differ by process. Authenticated gateways
serve both the Platform and direct clients, checking each key's scope and
allowed method groups. The Platform separately checks its members' roles and
session visibility. See [API keys and service access](../access-and-security/api-keys-and-service-access.md).

If multiple deployments share a Temporal namespace, set all three queue
variables independently. A deployment-specific `LIGHTSPEED_TASK_QUEUE` does
not change `LIGHTSPEED_TASK_QUEUE_BOTS` or
`LIGHTSPEED_TASK_QUEUE_CHANNELS`. Keep the connector host in the matching
Temporal namespace too.

## Secure the Temporal connection

Every runtime process and the connector host open their own connection to the
Temporal frontend. Workflow inputs and results travel over it, including
prompts and tool output, and whoever can reach the frontend can start, signal,
and read workflows in the namespace. With only `TEMPORAL_ADDRESS` and
`TEMPORAL_NAMESPACE` set, that connection is unencrypted and carries no
credential. Use that only for a frontend on a private network that nothing
else can reach.

For any other frontend, give both processes the same settings:

```dotenv
TEMPORAL_ADDRESS=temporal.internal.example:7233
TEMPORAL_NAMESPACE=lightspeed
TEMPORAL_TLS=true
TEMPORAL_TLS_SERVER_CA_FILE=/etc/lightspeed/temporal/ca.pem
TEMPORAL_TLS_CLIENT_CERT_FILE=/etc/lightspeed/temporal/client.pem
TEMPORAL_TLS_CLIENT_KEY_FILE=/etc/lightspeed/temporal/client.key
```

- **Server verification.** With TLS on, the server certificate must chain to
  the CA file, or to the operating system's trusted roots when no CA file is
  set, and must name the host in `TEMPORAL_ADDRESS`. Set
  `TEMPORAL_TLS_SERVER_NAME` when the certificate names a different host than
  the address the process dials.
- **Client certificate.** A frontend that requires mutual TLS gets the
  certificate and private key from the two client files. Both are needed.
- **API key.** A frontend that authenticates with API keys, such as a managed
  Temporal service, gets `TEMPORAL_API_KEY`, or `TEMPORAL_API_KEY_FILE` to read
  the key from a mounted secret. Setting a key turns TLS on; no CA file is
  needed when the service's certificate is publicly trusted. A client
  certificate and an API key can be combined when the frontend asks for both.

Supply the private key and the API key from your secret manager and mount the
files read-only. They are read when a process connects, so restart the runtime
processes and the connector host after rotating one. A setting that
contradicts another, or a file that is missing or holds the wrong kind of
content, stops the process at startup with an error that names the variable.
The [environment-variable reference](../reference/environment-variables.md#temporal-connection-security)
lists each setting and the rule that decides whether TLS is on.

Custom workers that run your own workflow tools connect with your Temporal
client configuration, not with these variables. Give them equivalent settings.

## Distinguish public URLs from internal routes

Several addresses can refer to the same installation while serving different
callers:

| Setting | Example | Caller and purpose |
| --- | --- | --- |
| `LIGHTSPEED_GATEWAY_BIND` | `0.0.0.0:18080` | Address inside the runtime's network namespace where HTTP listens. |
| `LIGHTSPEED_API_URL` on Platform | `http://lightspeed-runtime:18080/rpc` | Private Platform-to-runtime JSON-RPC route. |
| `LIGHTSPEED_PLATFORM_BASE_URL` | `https://lightspeed.example.com` | Public browser origin used for Platform authentication. |
| `LIGHTSPEED_PUBLIC_BASE_URL` | `https://lightspeed.example.com` | Public runtime origin used for OAuth callbacks, bot hooks, and environment addresses. Registered daemons must dial this origin, or one listed in `LIGHTSPEED_ENVIRONMENT_REGISTRATION_URLS`, because they sign it when they register. |
| `LIGHTSPEED_ENVIRONMENT_GATEWAY_URL` | `http://lightspeed-runtime:18080` | Private base URL workers use for environment routes; omit `/rpc`. |

An address containing `localhost` is relative to the process using it. Inside
a container it refers to that container. Conversely, binding to `0.0.0.0`
does not make that a useful public callback URL. Set the external origins
to the public HTTPS addresses when using a reverse proxy.

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

Provider API keys can be stored per universe through **Models → Add provider**.
This keeps the credential associated with the team that uses
it. Deployment-level `OPENAI_API_KEY` and `ANTHROPIC_API_KEY` are fallback
credentials when the corresponding universe record is absent. A disabled or
broken stored record blocks fallback.

Choose each universe's agent and speech-to-text defaults under **Models →
Defaults**, or through `lightspeed model defaults`. Each selection names a
provider, API kind, and model. New sessions use an explicit session model,
then the profile model, then the universe default; existing sessions keep
their resolved model. There is no deployment-wide model fallback.

Remove `LIGHTSPEED_CHAT_PROVIDER` and `LIGHTSPEED_CHAT_MODEL` from older
configuration: the runtime rejects them at startup. See
[Models and credentials](../using-lightspeed/models-and-credentials.md#choose-universe-defaults)
for setup and precedence.

## Choose the blob backend

With all `LIGHTSPEED_OBJECT_STORE_*` variables absent, blobs up to 64 KiB are
stored inline in PostgreSQL. Larger writes fail. This supports an initial
small-text walkthrough; configure S3-compatible or Azure Blob storage before working with
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

Existing S3 settings remain valid without a backend selector. To use native
Azure Blob storage instead:

```dotenv
LIGHTSPEED_OBJECT_STORE_BACKEND=azure
LIGHTSPEED_OBJECT_STORE_AZURE_ACCOUNT=yourstorageaccount
LIGHTSPEED_OBJECT_STORE_AZURE_CONTAINER=lightspeed-content
LIGHTSPEED_OBJECT_STORE_PREFIX=production
```

Choose how the Runtime authenticates. With a static credential, supply exactly
one of `LIGHTSPEED_OBJECT_STORE_AZURE_ACCESS_KEY` or
`LIGHTSPEED_OBJECT_STORE_AZURE_SAS_TOKEN` through the deployment's secret
mechanism. Where account keys and SAS are prohibited, use
[workload identity](#azure-workload-identity). Create the container first. Do not carry S3 bucket, region, or
path-style settings into this configuration. The client uses the account's
standard HTTPS Blob endpoint unless `LIGHTSPEED_OBJECT_STORE_ENDPOINT` is set.
For an emulator, the endpoint can be a loopback HTTP account URL, such as
`http://127.0.0.1:10000/account`; remote Azure endpoints require HTTPS. Put SAS
tokens in their dedicated secret setting rather than the endpoint URL.

Setting any variable in this group activates its configuration requirements,
even if the value is empty. S3 requires a nonempty bucket; Azure requires its
account, container, and credential. Remove the entire group when intentionally
using only PostgreSQL.

With object storage configured, small blobs still remain inline in PostgreSQL;
larger blobs go to the object store. PostgreSQL keeps their catalog and exact
physical object keys. A prefix change affects new keys, while existing records
continue to reference their original keys. Keep those objects accessible.
Changing the backend or container does not copy existing objects. Plan a storage
migration separately rather than switching an existing deployment's settings.

Remote PostgreSQL connections support TLS in the release build. Use
`sslmode=verify-full` in both Runtime and Platform database URLs, keeping
username/password authentication when that is the deployment's chosen method.
Normal HTTPS/TLS uses the clients' existing CA trust; manual certificate files
are needed only if a private authority must be added to that trust.
Changing a bucket or endpoint does not copy existing content, so plan that
change as a data migration and test old attachments and workspace files as
well as newly written content.

`LIGHTSPEED_BLOB_CACHE_BYTES` controls a per-process cache. Increasing worker
replicas also increases the aggregate cache budget. Blob collection is a
separate policy, described in [operations](operations.md#manage-retention-and-blob-collection).

### Azure workload identity

Workload identity authenticates to Blob storage with a projected Kubernetes
service account token, so the deployment holds no account key, SAS token, or
client secret. Select it explicitly:

```dotenv
LIGHTSPEED_OBJECT_STORE_BACKEND=azure
LIGHTSPEED_OBJECT_STORE_AZURE_AUTH_MODE=workload_identity
LIGHTSPEED_OBJECT_STORE_AZURE_ACCOUNT=yourstorageaccount
LIGHTSPEED_OBJECT_STORE_AZURE_CONTAINER=lightspeed-content
LIGHTSPEED_OBJECT_STORE_PREFIX=production

AZURE_CLIENT_ID=<client-id>
AZURE_TENANT_ID=<tenant-id>
AZURE_FEDERATED_TOKEN_FILE=<absolute-path-to-projected-token>
```

The Azure Workload Identity webhook normally injects the `AZURE_*` values and
mounts the token. Set `AZURE_AUTHORITY_HOST` only for a cloud other than the
public one. On the Azure side, add a federated credential to the identity for
the Runtime's service account, and grant the identity a data-plane role that
can read, write, list, and delete blobs in the container, such as Storage Blob
Data Contributor.

The Runtime exchanges the projected token for an Azure Storage access token when
it first needs one, then renews it as it nears expiry. Each exchange reads the
token file again, so rotation does not require a restart. Commands that use only
the database, such as migrations, do not read the file or contact Microsoft
Entra. Consequently, a wrong token path or a missing federated credential
appears at the first Blob operation rather than at startup. The error names the
stage: the token file, the Entra exchange with its `AADSTS` code, or the Blob
request with its storage error code and request ID.

This mode rejects an access key or SAS token rather than choosing between them,
and it never falls back to another credential. Existing objects, keys, and
prefixes stay valid when an account moves to this mode; no data migration is
needed.

### PostgreSQL workload identity

The Runtime and Platform can log in to Azure Database for PostgreSQL with a
Microsoft Entra token instead of a stored password. Each selects this separately,
and the URL names the managed identity as its user and holds no password:

```dotenv
LIGHTSPEED_POSTGRES_AUTH_MODE=workload_identity
LIGHTSPEED_POSTGRES_URL=postgres://<identity-name>@<server>.postgres.database.azure.com:5432/lightspeed?sslmode=verify-full

LIGHTSPEED_PLATFORM_DATABASE_AUTH_MODE=workload_identity
LIGHTSPEED_PLATFORM_DATABASE_URL=postgres://<identity-name>@<server>.postgres.database.azure.com:5432/platform?sslmode=verify-full
```

The user is the identity's name, not its client ID. Both processes also need
`AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, and `AZURE_FEDERATED_TOKEN_FILE`, which
Blob storage shares in the Runtime. Set `AZURE_AUTHORITY_HOST` only for a cloud
other than the public one. `sslmode=verify-full` is required because the token is
sent as a password, so the server's identity must be verified first.

On the database side, an Entra administrator creates a role for each identity and
gives it the database:

```sql
SELECT * FROM pgaadauth_create_principal('<identity-name>', false, false);
GRANT CONNECT ON DATABASE lightspeed TO "<identity-name>";
ALTER DATABASE lightspeed OWNER TO "<identity-name>";
```

In an existing database, also reassign the objects the previous role owns with
`REASSIGN OWNED`. Run the Runtime's migration job with the same identity as the
Runtime; the Platform migrates at startup.

Only opening a connection needs a current token, and open connections are not
affected when one expires. The Runtime renews its token at half of its lifetime,
and the Platform lets the Azure SDK renew its token as it nears expiry. Both
depend on Microsoft Entra to open a connection at startup. During a later outage
they can still open connections until the current token expires, and they keep
using the ones already open. This mode applies only to the Runtime and Platform;
Temporal is configured separately.

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
runtime and the deployment's Temporal namespace, reached with the same
`TEMPORAL_*` settings as the runtime
([Secure the Temporal connection](#secure-the-temporal-connection)). Its deployment-scoped key needs
the `deployment/channels`, `auth/lease`, `channels/inbound`, and `blobs/put`
method groups for account discovery, credential leasing, inbound admission,
and attachment uploads. Provider bot tokens are leased from core rather than
configured on the connector host.

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

An agent's runtime MCP connection is a stored universe resource. To reach a
private endpoint, both the server record's `allowPrivateNetwork` and the
deployment's `LIGHTSPEED_MCP_PRIVATE_NETWORKS` must permit it. OAuth metadata,
client registration, and token requests use the same list, decided for each
endpoint by its own host: list an internal authorization server as well as
the MCP server it protects. Without an entry, OAuth traffic is public HTTPS
only.

Configurator is a separately deployed MCP server that manages Lightspeed. Its
mode must match its upstream gateway, and a non-loopback listener requires an
explicit allowed-host configuration. The Platform's Configurator URL enables
its setup flow; it does not start the service. The setup provisions a dedicated
universe key for configuring resources and stores it as an outbound credential.
That key has no session or VFS access. See [Agent and tool access](../access-and-security/agent-and-tool-access.md)
for what attaching Configurator lets an agent do.

See [Tools and MCP](../using-lightspeed/tools-and-mcp.md) and the
[Configurator variables](../reference/environment-variables.md#configurator-mcp)
before enabling those paths.

## Verify a configuration change

First confirm the affected process starts with the intended settings. Then
test the capability that changed: read existing content after a storage
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
