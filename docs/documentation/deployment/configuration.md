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

Set `LIGHTSPEED_POSTGRES_URL` on the runtime and
`LIGHTSPEED_PLATFORM_DATABASE_URL` on Platform. Each application has its
own database and migrations. Keep agent and provider configuration in its
universe. Use environment variables or protected files for deployment
settings, and restart the process that reads a setting after changing it.

Docker's `--env-file` takes `KEY=value` lines. Use shell syntax in files
loaded by a shell. The Rust runtime and CLI can also load a `.env` file
from the current directory or its parents. Run production services from a
directory set aside for that installation.

## Keep deployment-wide settings aligned

| Setting group | What must agree across the deployment |
| --- | --- |
| Runtime storage | PostgreSQL, object storage backend, and bucket or container and prefix. |
| Temporal | Address, namespace, TLS and credentials, and the task queue for each worker role. |
| Secrets | The master key used to encrypt and resolve stored credentials. |
| Environment routing | The shared environment gateway URL and routing token. |
| Runtime API authentication | The configured runtime token set on every application gateway replica, and matching tokens in trusted clients. |

Give each deployment its own task queues if it shares a Temporal namespace.
Set each queue independently, including `LIGHTSPEED_TASK_QUEUE_CODE` if you
run Code Mode. See [the task queue reference](../reference/environment-variables.md)
for the variable names.

## Set Platform defaults

A Platform Admin can open **Admin → Platform → Settings** to control
personal organization creation and set the default universe limits. Personal
organizations are enabled by default. The initial limits are 25 universes
per personal organization and 100 per shared organization. Turning personal
organization creation off stops new ones from being created. Existing
organizations remain available.

Each setting shows where its value comes from. An environment variable
locks the setting in the UI. Otherwise the Platform Admin's saved value
applies, or the built-in default after **Reset to default**. Set a particular
organization's own limit in its **Admin → Settings** page and choose
**Use the default** to make it follow the deployment setting again.

For hosted member access, configure [Company sign-in (SSO)](../access-and-security/single-sign-on.md)
and review [API keys and service access](../access-and-security/api-keys-and-service-access.md).
The [Platform environment settings](../reference/environment-variables.md#platform-backend)
cover sign-in, session lifetimes, and API key expiry.

## Secure the Temporal connection

Every runtime process opens its own connection to the
Temporal frontend. Workflow inputs and results travel over it, including
prompts and tool output, and whoever can reach the frontend can start, signal,
and read workflows in the namespace. With only `TEMPORAL_ADDRESS` and
`TEMPORAL_NAMESPACE` set, that connection is unencrypted and carries no
credential. Use that only for a frontend on a private network that nothing
else can reach.

Use TLS when the frontend leaves that private boundary. Give each runtime process the same settings:

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
files read-only. They are read when a process connects, so restart the affected runtime processes after rotating one. A setting that
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
| `LIGHTSPEED_GATEWAY_BIND` | `0.0.0.0:18080` | Private HTTP API listener (`--bind`; default `127.0.0.1:18080`). |
| `LIGHTSPEED_PUBLIC_BIND` | `0.0.0.0:18082` | OAuth callback, client metadata, and bot webhook listener (`--public-bind`; default `127.0.0.1:18082`). |
| `LIGHTSPEED_ENVIRONMENT_GATEWAY_BIND` | `0.0.0.0:18083` | Daemon connections and private worker routes (`--environment-bind`; default `127.0.0.1:18083`). |
| `LIGHTSPEED_API_URL` on Platform | `http://lightspeed-runtime:18080/api/v1` | Private Platform-to-runtime HTTP API base URL. |
| `LIGHTSPEED_PLATFORM_BASE_URL` | `https://lightspeed.example.com` | Public browser origin used for Platform authentication. |
| `LIGHTSPEED_PUBLIC_BASE_URL` | `https://lightspeed.example.com` | OAuth callback and bot webhook origin; defaults to the public listener address. |
| `LIGHTSPEED_ENVIRONMENT_PUBLIC_BASE_URL` | `https://lightspeed.example.com` | Daemon connection origin; defaults to the environment listener address. Daemons must dial this origin, or one listed in `LIGHTSPEED_ENVIRONMENT_REGISTRATION_URLS`, because they sign it when they register. |
| `LIGHTSPEED_ENVIRONMENT_GATEWAY_URL` | `http://lightspeed-runtime:18083` | Private base URL workers use for environment routes; omit `/api/v1`. |

An address containing `localhost` is relative to the process using it. Inside
a container it refers to that container. Conversely, binding to `0.0.0.0`
does not make that a useful public callback URL. Set the external origins
to the public HTTPS addresses when using a reverse proxy.

Preserve paths, query strings, host, and scheme as described in
[Configure the public edge](self-hosting.md#configure-the-public-edge).
Daemon control and data routes require WebSocket upgrades and long-lived
connections. Workers also need the private environment route even when the
public WebSockets are reachable. All three listeners serve `/health` and
`/ready`; keep the application API listener private even when callbacks and daemon
connections are publicly reachable.

The internal environment token is separate from an API token or a daemon
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

Keep `LIGHTSPEED_PLATFORM_AUTH_SECRET` stable across Platform restarts.
It protects sign-in state. See
[the recovery inventory](upgrades-and-recovery.md#preserve-a-complete-recovery-set).

Configure [models and credentials](../using-lightspeed/models-and-credentials.md)
per universe, including the default model for agent runs and, if needed,
speech-to-text.

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

Run `lightspeed-envd` on the machine that supplies compute and configure
it there using `LIGHTSPEED_ENVD_*`. Keep its state directory when its
registered identity needs to survive restarts. See
[Bring your own compute](../environments/bring-your-own-compute.md).

Register a provisioner under **Admin → Platform → Environment providers**.
It can serve every universe or be limited to selected ones. Follow the
[Incus setup guide](../environments/incus-vms.md) for a working provider.

### MCP and Configurator

For an agent to reach a private MCP server, allow private network access
on the server record and list the endpoint in
`LIGHTSPEED_MCP_PRIVATE_NETWORKS`. List any separate internal OAuth
authorization server as well. See [Tools and MCP](../using-lightspeed/tools-and-mcp.md).

If you run Configurator, point it to Platform's `/api/v1` and configure
Platform with the MCP service URL. The managed setup gives the agent a
restricted Platform service-user key. See
[Configurator MCP](../integrating-and-extending/configurator-mcp.md).

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
