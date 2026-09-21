# Self-host Lightspeed

This guide installs the Lightspeed runtime and Platform web app on one Linux
x86_64 host, using existing PostgreSQL and Temporal services. It installs the
published images from one release, keeps the runtime API
private, and exposes the web app through your HTTPS reverse proxy.

The [deployment overview](overview.md) explains the component boundaries.
This recipe initially stores small blobs in PostgreSQL. The current inline
limit is 64 KiB per blob; larger writes require S3-compatible object storage.
Use the small-text verification below, then configure object storage before
using larger files, attachments, or payloads. Chat connectors, Configurator
MCP, and execution environments can be added afterward.

## Prepare the infrastructure

Before starting the application, provide:

- A Linux x86_64 application host with Docker, Bash, standard command-line
  utilities, curl, jq, and OpenSSL. Installing published images requires no
  Rust or Node.js toolchain on the host.
- Two empty PostgreSQL databases, called `lightspeed` and
  `lightspeed_platform` in this example. Each application's database user
  needs permission to create and migrate its schema. The databases can share
  a PostgreSQL server, but keep their records and migration histories separate.
- A running Temporal service with a precreated namespace, called `lightspeed`
  here. Its frontend must be reachable from the application containers over
  your private network. Manage its persistence and backups as part of that
  service.
- A DNS name and an HTTPS reverse proxy on the application host, with a valid
  certificate. This guide uses `lightspeed.example.com` as a placeholder.
- Outbound access to GitHub Releases, GitHub Container Registry, and the model
  providers you plan to use.

Use infrastructure addresses that resolve and are reachable inside Docker.
`localhost` in a container refers to that container. The runtime's current
Temporal configuration exposes an address and namespace; it does not configure
Temporal API-key or client-certificate authentication. This recipe assumes a
private Temporal frontend compatible with that connection.

The development launcher's Temporal service uses a development-server
configuration. Provision the durable infrastructure above separately for this
installation.

## Download one release

Choose a tag from [GitHub Releases](https://github.com/smartcomputer-ai/lightspeed/releases).
The example below uses `v0.2.0`; replace it with the release you intend to
install. Download its manifest into a directory you will keep with the
deployment record:

```bash
LIGHTSPEED_RELEASE_TAG=v0.2.0
mkdir -p "$HOME/lightspeed-releases/$LIGHTSPEED_RELEASE_TAG"
cd "$HOME/lightspeed-releases/$LIGHTSPEED_RELEASE_TAG"
curl --fail --location --output release-manifest.json \
  "https://github.com/smartcomputer-ai/lightspeed/releases/download/$LIGHTSPEED_RELEASE_TAG/release-manifest.json"
```

Pull the runtime and Platform images by the digests recorded in that manifest.
The Platform image includes the web app. Give them local names for the
remaining commands in this guide:

```bash
LIGHTSPEED_RELEASE_ID="$LIGHTSPEED_RELEASE_TAG"
LIGHTSPEED_RUNTIME_IMAGE="$(jq -er '.images.runtime' release-manifest.json)"
LIGHTSPEED_PLATFORM_IMAGE="$(jq -er '.images.platform' release-manifest.json)"
docker pull "$LIGHTSPEED_RUNTIME_IMAGE"
docker pull "$LIGHTSPEED_PLATFORM_IMAGE"
docker tag "$LIGHTSPEED_RUNTIME_IMAGE" "lightspeed-runtime:$LIGHTSPEED_RELEASE_ID"
docker tag "$LIGHTSPEED_PLATFORM_IMAGE" "lightspeed-platform:$LIGHTSPEED_RELEASE_ID"
```

Keep using this shell, or set `LIGHTSPEED_RELEASE_ID` to the same value in a
new shell. Continue at [Configure the two applications](#configure-the-two-applications).
Keep all components on the same release, and use
[Upgrades and recovery](upgrades-and-recovery.md) when updating an existing
installation.

### Build one release instead

If you need an unreleased change or your own build, use the source path below.
In addition to Docker and the utilities above, it needs GNU Make, Git, and
Node.js 24 or newer. The release build container supplies the Rust compiler
and package-build dependencies.

Use a fresh release checkout. Replace `RELEASE_REF` below with the exact tag
or commit you intend to deploy:

```bash
git clone https://github.com/smartcomputer-ai/lightspeed.git lightspeed-release
cd lightspeed-release
git checkout --detach RELEASE_REF
make release
```

The build produces `dist/` and local images, including
`lightspeed-local-runtime` and `lightspeed-local-platform`. It performs
packaging and image checks. Keep the source revision and release manifest
with your deployment record; the build checks do not replace the application
verification below.

Give the two images revision-specific local names so a later build using the
default local tags does not change the names used by this installation:

```bash
LIGHTSPEED_RELEASE_ID="$(git rev-parse --short=12 HEAD)"
docker tag lightspeed-local-runtime "lightspeed-runtime:$LIGHTSPEED_RELEASE_ID"
docker tag lightspeed-local-platform "lightspeed-platform:$LIGHTSPEED_RELEASE_ID"
```

Keep using this shell for the remaining commands, or set `LIGHTSPEED_RELEASE_ID`
to the same value in a new shell. If you distribute your own images through a
registry, pin the component digests from the same release. The [release guide](../../releasing.md)
describes that artifact and publication contract.

## Configure the two applications

Create a private deployment directory outside the source checkout:

```bash
mkdir -p "$HOME/lightspeed-deployment"
chmod 700 "$HOME/lightspeed-deployment"
cd "$HOME/lightspeed-deployment"
umask 077
touch runtime.env platform.env
chmod 600 runtime.env platform.env
```

Generate the runtime's secrets master key:

```bash
openssl rand -base64 32
```

Generate two other secrets, one for Platform authentication and one for
internal environment routing, by running this command once for each:

```bash
openssl rand -hex 32
```

Keep all three values stable across restarts. Store them in your secret manager
and recovery material. Replacing the runtime's master key makes credentials
encrypted with the previous key unreadable.

Edit `runtime.env` with the following settings, replacing every angle-bracket
placeholder. These are Docker environment files: use `KEY=value` lines with
no `export` prefix and no shell quotes around values.

```dotenv
LIGHTSPEED_POSTGRES_URL=<runtime-postgres-connection-url>
LIGHTSPEED_AUTH_MODE=authenticated
LIGHTSPEED_GATEWAY_BIND=0.0.0.0:18080
LIGHTSPEED_PUBLIC_BASE_URL=https://lightspeed.example.com
LIGHTSPEED_ENVIRONMENT_GATEWAY_URL=http://lightspeed-runtime:18080
LIGHTSPEED_ENVIRONMENT_GATEWAY_TOKEN=<internal-routing-secret>
LIGHTSPEED_SECRETS_MASTER_KEY=<base64-master-key>
TEMPORAL_ADDRESS=<private-temporal-host>:7233
TEMPORAL_NAMESPACE=lightspeed
LIGHTSPEED_ROLES=gateway,environment-gateway,sessions,bots,channels
LIGHTSPEED_LOG_FORMAT=json
```

A database URL has the form
`postgres://USER:PASSWORD@HOST:5432/lightspeed`; URL-encode special characters
in credentials. Supply the connection options required by your PostgreSQL
deployment.

The public base URL generates addresses that external clients need, such as
OAuth callbacks and daemon data connections. The separate environment gateway
URL keeps worker-to-environment traffic on the Docker network. Both values
are needed here because the public proxy will expose only selected runtime
routes.

Leave all `LIGHTSPEED_OBJECT_STORE_*` settings absent for the initial small-blob
PostgreSQL setup. Writes above 64 KiB fail without object storage. Even setting
part of that configuration to an empty value can activate object-store
configuration and require a bucket. Follow
[Choose the blob backend](configuration.md#choose-the-blob-backend) to add the
complete S3-compatible configuration.

Provision the canonical Platform service and its key after runtime migration,
following [authentication and access](authentication-and-tenancy.md). The service
needs explicit deployment and universe authority for the operations it performs.
Then edit `platform.env`:

```dotenv
LIGHTSPEED_PLATFORM_DATABASE_URL=<platform-postgres-connection-url>
LIGHTSPEED_PLATFORM_AUTH_SECRET=<platform-auth-secret>
LIGHTSPEED_PLATFORM_BASE_URL=https://lightspeed.example.com
LIGHTSPEED_API_URL=http://lightspeed-runtime:18080/rpc
LIGHTSPEED_PLATFORM_API_KEY=<canonical-platform-service-key>
LIGHTSPEED_PLATFORM_ADMIN_EMAIL=<administrator-email>
LIGHTSPEED_PLATFORM_ADMIN_PASSWORD=<strong-initial-password>
PORT=3000
```

Use the `lightspeed_platform` database in this file. The public base URL must
match the browser origin, including `https://`. Same-origin use needs no
additional trusted browser origins.

The Platform creates the initial administrator only when its users table is
empty. Set the intended credentials before the first startup. Changing these
environment variables later does not reset an existing account's password.

This guide uses one dedicated Temporal namespace with the default task queues.
If several deployments share a namespace, also assign distinct values for
`LIGHTSPEED_TASK_QUEUE`, `LIGHTSPEED_TASK_QUEUE_BOTS`, and
`LIGHTSPEED_TASK_QUEUE_CHANNELS` and keep each deployment's processes aligned.

## Migrate and start the runtime

Create a Docker network for the application containers:

```bash
docker network create lightspeed
```

Run the runtime migration explicitly before starting the service:

```bash
docker run --rm --network lightspeed --env-file runtime.env \
  "lightspeed-runtime:$LIGHTSPEED_RELEASE_ID" migrate

docker run --rm --network lightspeed --env-file runtime.env \
  "lightspeed-runtime:$LIGHTSPEED_RELEASE_ID" schema-version
```

The migration command applies the release's embedded migrations and records
their checksums. The diagnostic command reports the schema revision. Normal
runtime startup verifies the ledger and refuses a schema that requires
migration; it does not apply migrations implicitly.

Start all runtime roles in one container:

```bash
docker run -d --name lightspeed-runtime \
  --network lightspeed \
  --restart unless-stopped \
  --stop-signal SIGINT --stop-timeout 30 \
  --env-file runtime.env \
  -p 127.0.0.1:18080:18080 \
  "lightspeed-runtime:$LIGHTSPEED_RELEASE_ID"
```

The host mapping is loopback-only. The Platform can reach the runtime by
container name on the Docker network, and the host proxy can reach its selected
public routes at `127.0.0.1:18080`. The explicit stop signal lets Docker request
the runtime's shutdown path.

Wait for the listener; the retries allow time for startup:

```bash
curl --fail --retry 30 --retry-connrefused --retry-delay 2 \
  http://127.0.0.1:18080/health
```

It should return `ok`. This is a liveness check; the completed run below
verifies that the workers and provider can do useful work.

## Start the Platform

```bash
docker run -d --name lightspeed-platform \
  --network lightspeed \
  --restart unless-stopped \
  --env-file platform.env \
  -p 127.0.0.1:3000:3000 \
  "lightspeed-platform:$LIGHTSPEED_RELEASE_ID"

curl --fail --retry 30 --retry-connrefused --retry-delay 2 \
  http://127.0.0.1:3000/health
```

The Platform applies its own migrations on startup, then bootstraps the
administrator if the database is new. Its health endpoint returns
`{"ok":true}`. The same image contains and serves the built web app under
`/app/`.

## Configure the public edge

Configure your existing HTTPS reverse proxy for
`https://lightspeed.example.com` using this routing table. Preserve the request
path and query string when forwarding. The upstream addresses below assume
the proxy runs on the application host.

| Public path | Upstream | Purpose |
| --- | --- | --- |
| Exact `/auth/callback` | `http://127.0.0.1:18080` | Runtime OAuth callback |
| Exact `/auth/client-metadata.json` | `http://127.0.0.1:18080` | Runtime OAuth client metadata |
| Prefix `/hooks/bots/` | `http://127.0.0.1:18080` | Bot webhooks |
| Exact `/environment-gateway/connect` | `http://127.0.0.1:18080` | Daemon control WebSocket |
| Exact `/environment-gateway/data` | `http://127.0.0.1:18080` | Daemon data WebSocket |
| `/rpc` and prefix `/environment-gateway/routes/` | Reject at the public edge | Private runtime and worker APIs |
| All remaining paths | `http://127.0.0.1:3000` | Platform, including `/app/`, `/api/auth/`, and `/api/v1/` |

Allow WebSocket upgrades and long-lived connections on the two public daemon
routes. Forward the original host and scheme through the proxy's forwarded
headers. Match those two routes exactly; forwarding the entire
`/environment-gateway/` prefix would also expose internal worker routes.

The Platform performs authentication and supplies trusted universe headers
on its calls to the runtime. An internet client must not be able to call the
runtime's authenticated RPC listener directly. Keep port `18080` private
even if your reverse proxy has its own authentication rules.

If the proxy runs in another container, put it on the application network and
use the service names as upstreams. Its own loopback address does not reach
the host mappings above.

## Verify a complete run

1. Open `https://lightspeed.example.com/app/` and sign in with the
   administrator you configured.
2. Choose **New universe** and create one for your team. The Platform creates
   the corresponding runtime universe as part of that operation.
3. Follow [Configure a model](../getting-started/quickstart.md#configure-a-model)
   to add a provider key and explicitly select a model for a new session.
4. Send a message and wait for the assistant's completed response.
5. Reload the session and confirm the conversation remains visible.

That sequence exercises the public edge, Platform authentication, both
application databases, runtime admission, Temporal workers, and model access.
The [first-agent walkthrough](../getting-started/first-agent.md) additionally
checks persistent file tools. To verify daemon routing, use
[Bring your own compute](../environments/bring-your-own-compute.md) with the
public `wss://` URL.

After bootstrap, remove the administrator password from the deployment
environment file and omit it when recreating the Platform container. Manage
the account through the Platform. Preserve its authentication secret and the
runtime's encryption key.

## Operate and update the installation

Inspect failures through the container logs:

```bash
docker logs --tail 100 lightspeed-runtime
docker logs --tail 100 lightspeed-platform
```

| Symptom | Likely boundary to inspect |
| --- | --- |
| Runtime exits before serving health | Database connectivity, migration ledger, required settings, or Temporal address/namespace |
| Platform health never succeeds | Platform database permissions/migrations and required authentication settings |
| Sign-in redirects or origin checks fail | Public base URL, HTTPS, and proxy host/scheme forwarding |
| A universe cannot be created | Platform-to-runtime connectivity and authenticated runtime configuration and Platform service permissions |
| A run is accepted but makes no progress | Temporal namespace, session workers, and matching task queues |
| Model calls fail | The universe's provider credential and the session's selected model |
| A daemon connects but process calls cannot route | The two public WebSocket paths, internal environment gateway URL/token, and the single environment-gateway process |

Use [Operations](operations.md) for monitoring, role scaling, and retention,
and [Troubleshooting](troubleshooting.md) for a failing request path.
[Upgrades and recovery](upgrades-and-recovery.md) gives the maintenance
procedure and the complete recovery inventory, including optional connector
and machine state.

[Authentication and access](authentication-and-tenancy.md) covers accounts and
client keys; [Multitenancy](multi-tenancy.md) explains universe isolation and
retirement. [Configuration](configuration.md) explains deployment choices,
with exact settings in the
[environment-variable reference](../reference/environment-variables.md).

## Download standalone binaries

Use the standalone archives when you want to manage the runtime process
directly, or need the CLI for an existing installation. The published server
and CLI target Linux x86_64 with glibc; they are not macOS, ARM64, or Alpine
binaries. The runtime image above supplies the Linux userspace for the
container installation.

In the directory containing the release manifest downloaded earlier, obtain
and verify the server and CLI archives:

```bash
for LIGHTSPEED_COMPONENT in server cli; do
  LIGHTSPEED_ARCHIVE="$(jq -er --arg component "$LIGHTSPEED_COMPONENT" \
    '.binaries[$component].file' release-manifest.json)" || break
  LIGHTSPEED_ARCHIVE_SHA="$(jq -er --arg component "$LIGHTSPEED_COMPONENT" \
    '.binaries[$component].sha256' release-manifest.json)" || break
  curl --fail --location --output "$LIGHTSPEED_ARCHIVE" \
    "https://github.com/smartcomputer-ai/lightspeed/releases/download/$LIGHTSPEED_RELEASE_TAG/$LIGHTSPEED_ARCHIVE" || break
  printf '%s  %s\n' "$LIGHTSPEED_ARCHIVE_SHA" "$LIGHTSPEED_ARCHIVE" \
    | sha256sum --check - || break
  tar -xzf "$LIGHTSPEED_ARCHIVE" || break
done
```

This extracts `lightspeed-server` and `lightspeed` into the current directory.
Their `--help` output lists the commands. The release page also provides
archives for the Incus provider and environment daemon; choose those from the
release matching your runtime when you need them.

A native server needs the same runtime configuration, PostgreSQL migrations,
and Temporal service as the container. Supply its environment through your
process supervisor, run `lightspeed-server migrate`, and then start
`lightspeed-server` with the configured roles. The `runtime.env` example above
uses container addresses; adapt those to the native process's network.
Platform remains a separate application for the web app and authentication.
Use [Configuration](configuration.md) for the runtime settings and
[Continue from the CLI](../using-lightspeed/sessions-and-runs.md#continue-from-the-cli)
for client setup.
