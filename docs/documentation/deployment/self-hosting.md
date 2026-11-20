# Self-host Lightspeed

This guide installs the Lightspeed runtime and Platform web app on one Linux
x86_64 host, using existing PostgreSQL and Temporal services. It keeps the
runtime API private and exposes the web app through your HTTPS reverse proxy.
Use images from one release, or build the current source when you need work
that has not yet been released.

The [deployment overview](overview.md) explains how the components fit together.
Use empty databases for a fresh installation. If you have retained work,
check [database compatibility](upgrades-and-recovery.md#check-compatibility-before-reusing-state)
first.

This recipe initially stores blobs up to 64 KiB in PostgreSQL. Add
[S3-compatible or Azure Blob storage](configuration.md#choose-the-blob-backend)
before using larger files or attachments. You can add execution environments
and [Configurator MCP](../integrating-and-extending/configurator-mcp.md)
after the first run.

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
`localhost` in a container refers to that container. This recipe assumes a
Temporal frontend on a private network that accepts unencrypted connections
without a credential, so it sets only an address and a namespace. If your
Temporal service requires TLS, a client certificate, or an API key, add the
settings from
[Secure the Temporal connection](configuration.md#secure-the-temporal-connection)
to `runtime.env`, and mount the certificate and key files it names into the
runtime container.

The development launcher's Temporal service uses a development-server
configuration. Provision the durable infrastructure above separately for this
installation.

## Download one release

Choose a tag from [GitHub Releases](https://github.com/smartcomputer-ai/lightspeed/releases)
and read its release notes. This manual describes current development; for an
older release, use its matching documentation and configuration. To install the
current code before a matching release exists, use [Build one release instead](#build-one-release-instead).

Replace `vX.Y.Z` below with the selected tag. Download its manifest into a
directory you will keep with the deployment record:

```bash
LIGHTSPEED_RELEASE_TAG=vX.Y.Z
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
Node.js 24.21.0 or newer. The release build container supplies the Rust compiler
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

Generate three other secrets: one for Platform authentication, one for
internal environment routing, and one for the Platform’s runtime HTTP access.
Run this command once for each:

```bash
openssl rand -hex 32
```

Keep all four values stable across restarts. Store them in your secret manager
and recovery material. Replacing the runtime's master key makes credentials
encrypted with the previous key unreadable.

Edit `runtime.env` with the following settings, replacing every angle-bracket
placeholder. These are Docker environment files: use `KEY=value` lines with
no `export` prefix and no shell quotes around values.

```dotenv
LIGHTSPEED_POSTGRES_URL=<runtime-postgres-connection-url>
LIGHTSPEED_AUTH_MODE=authenticated
LIGHTSPEED_API_TOKEN_PLATFORM=<platform-runtime-token>
LIGHTSPEED_GATEWAY_BIND=0.0.0.0:18080
LIGHTSPEED_PUBLIC_BIND=0.0.0.0:18082
LIGHTSPEED_ENVIRONMENT_GATEWAY_BIND=0.0.0.0:18083
LIGHTSPEED_PUBLIC_BASE_URL=https://lightspeed.example.com
LIGHTSPEED_ENVIRONMENT_PUBLIC_BASE_URL=https://lightspeed.example.com
LIGHTSPEED_ENVIRONMENT_GATEWAY_URL=http://lightspeed-runtime:18083
LIGHTSPEED_ENVIRONMENT_GATEWAY_TOKEN=<internal-routing-secret>
LIGHTSPEED_SECRETS_MASTER_KEY=<base64-master-key>
TEMPORAL_ADDRESS=<private-temporal-host>:7233
TEMPORAL_NAMESPACE=lightspeed
LIGHTSPEED_ROLES=gateway,environment-gateway,sessions,code
LIGHTSPEED_LOG_FORMAT=json
```

These roles run sessions, including Code Mode. Add the `bots` role if
your deployment uses bot workflows. See
[Runtime roles and scaling](overview.md#runtime-roles-and-scaling).

A database URL has the form
`postgres://USER:PASSWORD@HOST:5432/lightspeed`; URL-encode special characters
in credentials. Supply the connection options required by your PostgreSQL
deployment.

The public base URL generates OAuth callback and bot webhook addresses. The
environment public base URL generates daemon addresses and identifies the
origin daemons sign when registering. The private environment gateway URL
keeps worker-to-environment traffic on the Docker network. Set all three here
because the public proxy exposes only selected runtime routes.

Leave all `LIGHTSPEED_OBJECT_STORE_*` settings absent for the initial small-blob
PostgreSQL setup. Writes above 64 KiB fail without object storage. Even setting
part of that configuration to an empty value can activate object-store
configuration and require backend-specific settings. Follow
[Choose the blob backend](configuration.md#choose-the-blob-backend) to add the
complete S3-compatible or Azure Blob configuration.

The Platform uses the runtime token configured above. Give the same secret to
both applications, keeping it out of browser configuration, agent workspaces,
and environment credentials. Prepare `platform.env` with:

```dotenv
LIGHTSPEED_PLATFORM_DATABASE_URL=<platform-postgres-connection-url>
LIGHTSPEED_PLATFORM_AUTH_SECRET=<platform-auth-secret>
LIGHTSPEED_PLATFORM_BASE_URL=https://lightspeed.example.com
LIGHTSPEED_API_URL=http://lightspeed-runtime:18080/api/v1
LIGHTSPEED_PLATFORM_API_TOKEN=<platform-runtime-token>
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
If several deployments share a namespace, set a distinct task queue for each
worker role in each deployment. See
[Runtime configuration](configuration.md#keep-deployment-wide-settings-aligned).

## Migrate and start the runtime

Create a Docker network for the application containers:

```bash
docker network create lightspeed
```

Run the runtime migration before starting the service:

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

Runtime tokens come from configuration; migration creates no caller keys.
`authenticated` mode refuses startup without a token. Every configured token
has full authority over all universes and may name an actor. The first
Platform administrator is a separate login created from the email and
password above. See
[Runtime service access](../access-and-security/api-keys-and-service-access.md)
for mounted token files and rotation.

Start the configured runtime roles in one container.

```bash
docker run -d --name lightspeed-runtime \
  --network lightspeed \
  --restart unless-stopped \
  --stop-timeout 30 \
  --env-file runtime.env \
  -p 127.0.0.1:18080:18080 \
  -p 127.0.0.1:18082:18082 \
  -p 127.0.0.1:18083:18083 \
  "lightspeed-runtime:$LIGHTSPEED_RELEASE_ID"
```

The host mappings are loopback-only. The Platform reaches private HTTP API by
container name on the Docker network. The host proxy reaches public callbacks
at `127.0.0.1:18082` and daemon connections at `127.0.0.1:18083`. Docker's
stop signal starts graceful shutdown; the timeout gives it time to finish.

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
| Exact `/ingress/credentials/callback` | `http://127.0.0.1:18082` | Runtime OAuth callback |
| Exact `/ingress/credentials/client-metadata.json` | `http://127.0.0.1:18082` | Runtime OAuth client metadata |
| Prefix `/ingress/webhooks/bots/` | `http://127.0.0.1:18082` | Bot webhooks |
| Exact `/environments/connect` | `http://127.0.0.1:18083` | Daemon control WebSocket |
| Exact `/environments/data` | `http://127.0.0.1:18083` | Daemon data WebSocket |
| Prefixes `/environments/routes/` and `/environments/peer/` | Reject at the public edge | Private environment worker and peer routes |
| All remaining paths | `http://127.0.0.1:3000` | Platform, including `/app/`, `/api/auth/`, and `/api/v1/` |

Use the listed paths when registering OAuth redirects, webhook senders, and
registered daemons. If you upgrade from an installation with older paths,
update the proxy and callback settings together.

Allow WebSocket upgrades and long-lived connections on the two public daemon
routes. Forward the original host and scheme through the proxy's forwarded
headers, pass WebSocket ping frames through, accept messages of up to 16 MiB,
and do not close a quiet connection sooner than 60 seconds. Match those two
routes exactly; forwarding the entire `/environments/` prefix would
also expose internal worker routes. Daemons must dial these routes on the
host of `LIGHTSPEED_ENVIRONMENT_PUBLIC_BASE_URL`, or on a URL listed in
`LIGHTSPEED_ENVIRONMENT_REGISTRATION_URLS`: they sign that origin when they
register, and the runtime refuses to serve registration on a non-loopback
bind with neither.

Keep runtime port `18080` on the private application network. Platform
uses it after authorizing the request. Send browsers, API clients and CLI
users to the public Platform URL so their session or Platform API key is
checked there. Reserve direct runtime access for trusted infrastructure
components. See [API keys and service access](../access-and-security/api-keys-and-service-access.md).

If the proxy runs in another container, put it on the application network and
use the service names as upstreams. Its own loopback address does not reach
the host mappings above.

## Verify a complete run

1. Open `https://lightspeed.example.com/app/` and sign in with the
   administrator you configured.
2. Choose **New universe** in the switcher and create one in your personal
   organization. For a shared space, create an organization from **Admin →
   Platform → All Organizations**, then create its universe.
3. Follow [Configure a model](../getting-started/quickstart.md#configure-a-model)
   to add a provider key and select a model for a new session.
4. Send a message and wait for the assistant's completed response.
5. Reload the session and confirm the conversation remains visible.

That sequence exercises the public edge, Platform authentication, both
application databases, runtime admission, Temporal workers, and model access.
The [first-agent walkthrough](../getting-started/first-agent.md) additionally
checks persistent file tools. To verify daemon routing, use
[Bring your own compute](../environments/bring-your-own-compute.md) with the
public `wss://` URL.

After bootstrap, remove the administrator password from the deployment
environment file and omit it when recreating the Platform container. Preserve the Platform authentication secret and runtime encryption key.
For company login and emergency access, see
[Company sign-in (SSO)](../access-and-security/single-sign-on.md).

## Operate and update the installation

Inspect failures through the container logs:

```bash
docker logs --tail 100 lightspeed-runtime
docker logs --tail 100 lightspeed-platform
```

| Symptom | What to inspect |
| --- | --- |
| Runtime exits before serving health | Database connectivity, migration ledger, required settings, or Temporal address, namespace, and TLS or API key settings |
| Platform health never succeeds | Platform database permissions/migrations and required authentication settings |
| Sign-in redirects or origin checks fail | Public base URL, HTTPS, and proxy host/scheme forwarding |
| A universe cannot be created | Create access and an available slot in the organization, then Platform-to-runtime connectivity and the matching runtime token |
| A run is accepted but makes no progress | Temporal namespace, session workers, and matching task queues |
| Model calls fail | The universe's provider credential and the session's selected model |
| A daemon connects but process calls cannot route | The two public WebSocket paths, internal environment gateway URL/token, and replica peer reachability and leases |

Use [Operations](operations.md) for monitoring, role scaling, and retention,
and [Troubleshooting](troubleshooting.md) for a failing request path.
[Upgrades and recovery](upgrades-and-recovery.md) gives the maintenance
procedure and the complete recovery inventory.

[Access and security](../access-and-security/overview.md) covers organizations, roles,
runtime tokens, and isolation; [Managing universes](multi-tenancy.md) explains
universe reconciliation and retirement. [Configuration](configuration.md)
explains deployment choices, with exact settings in the
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

This extracts `lightspeed-runtime` and `lightspeed` into the current directory.
Their `--help` output lists the commands. The release page also provides
archives for the Incus provider and environment daemon; choose those from the
release matching your runtime when you need them.

A native server needs the same runtime configuration, PostgreSQL migrations,
and Temporal service as the container. Supply its environment through your
process supervisor, run `lightspeed-runtime migrate`, and then start
`lightspeed-runtime` with the configured roles. The `runtime.env` example above
uses container addresses; adapt those to the native process's network.
Platform remains a separate application for the web app and authentication.
Use [Configuration](configuration.md) for the runtime settings and
[Continue from the CLI](../using-lightspeed/sessions-and-runs.md#continue-from-the-cli)
for client setup.
