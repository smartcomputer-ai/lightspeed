# API keys and runtime tokens

The runtime authenticates trusted components with tokens from configuration.
Every runtime token has full authority over every universe and method. People
use the Platform, which checks membership, roles and resource visibility. A
standalone runtime needs its own trusted gateway or a trusted network.

## Platform API keys

Create personal keys under **Your profile → API keys** and application keys
under **Accounts → Applications → API keys**. Better Auth stores hashed keys,
checks expiry and revocation, and maintains usage counters. The Platform
binds each key to its user or application and applies current permissions.
Application keys use Better Auth organization ownership, but the application
association is server-controlled: an organization key alone grants no access.

Keys expire after 365 days by default, with a deployment-configurable maximum.
Copy a new secret when it is shown; it is never returned again. For rotation,
create a replacement, move consumers, and explicitly revoke the previous key.
Keys cannot mint other keys. Human browser and CLI sessions manage credentials.

Send `Authorization: Bearer <key>` to the Platform `/api/v1` base URL. Runtime
resource routes are shared with the direct runtime API; native account,
application, grant, and credential operations are under `/api/v1/platform`.
Both SDKs use `client.call` for runtime and native Platform operation IDs.
Generated `client.platform` helpers provide typed native parameter objects
without a separate transport or authentication path. `lightspeed connect login --url DEPLOYMENT_URL`
uses browser-approved device authorization for a human CLI session;
`lightspeed connect add` accepts API keys. See [CLI connections](../using-lightspeed/cli.md#connect-to-platform).

Local scripts can reuse a saved CLI connection through the Node-only
`createClientFromConnection` factory from `@lightspeed-ai/sdk/node` or Python's
`await AsyncLightspeedClient.from_connection(...)`. These factories reread the
saved credential per request and renew CLI-owned session credentials without
starting login or rewriting externally managed secret files. The selected
account remains explicit in native helper parameters. See
[Reuse a saved CLI connection](../integrating-and-extending/api-and-typescript.md#reuse-a-saved-cli-connection).

Optional account IDs, universe UUIDs, operation IDs, and read-only restrictions
narrow a key's authority. They never grant permissions its principal lacks.
Universe creation also requires an account Creator or Admin role and the key's
creation permission. A key restricted to existing universes cannot create one.
Application keys belong to one account and use that application's grants,
independently of their human creator's continued access.

Local personal keys and application keys do not depend on a browser session.
Company users require admission refreshed within the configured company
window until an authoritative background directory source is available.
Suspension and application disabling are checked on subsequent requests.

The following sections describe **runtime infrastructure tokens**, which have
full deployment authority and must remain with trusted infrastructure.

## Gateway modes

`LIGHTSPEED_AUTH_MODE` is required:

| Mode | Behavior |
| --- | --- |
| `authenticated` | Requires at least one configured API token. Invalid credentials receive HTTP 401 before the request body is read. |
| `single` | Unauthenticated local development, using `LIGHTSPEED_PG_UNIVERSE_ID`. Credential, universe and actor headers are rejected. |

The runtime binds separate listeners:

| Listener | Default | Routes |
| --- | --- | --- |
| Private application API | `127.0.0.1:18080` (`LIGHTSPEED_GATEWAY_BIND`) | `/api/v1/...` |
| Public callbacks and webhooks | `127.0.0.1:18082` (`LIGHTSPEED_PUBLIC_BIND`) | `/ingress/credentials/callback`, `/ingress/credentials/client-metadata.json`, `/ingress/webhooks/bots/…` |
| Environment gateway | `127.0.0.1:18083` (`LIGHTSPEED_ENVIRONMENT_GATEWAY_BIND`) | Daemon connections, worker routes, replica peers |

Each serves `/health` and `/ready`. The `gateway` role starts the first two;
`environment-gateway` starts the third. Bind addresses must be distinct, with
fixed nonzero ports. Network rules should admit only trusted components and
operators to the application API. Publish callbacks and webhooks using `LIGHTSPEED_PUBLIC_BASE_URL`.
Daemon origins use `LIGHTSPEED_ENVIRONMENT_PUBLIC_BASE_URL` and optional
`LIGHTSPEED_ENVIRONMENT_REGISTRATION_URLS`; keep worker and peer routes private.

## Configure tokens

Use an inline secret or a mounted file:

```text
LIGHTSPEED_AUTH_MODE=authenticated
LIGHTSPEED_API_TOKEN_FILE=/run/secrets/runtime-token
```

The plain token is named `default`. Optional names distinguish components in
attribution without changing their authority:

```text
LIGHTSPEED_API_TOKEN_PLATFORM_FILE=/run/secrets/platform-token
LIGHTSPEED_API_TOKEN_OPERATOR_FILE=/run/secrets/operator-token
```

A suffix becomes a lower-case token name. Secrets must contain at least 32
non-whitespace printable ASCII characters suitable for an HTTP header. Use a cryptographic
random source; `lightspeed token generate-secret` works offline. File input
removes one terminal LF or CRLF. Other bytes are significant.

Duplicate names, duplicate secrets, or both inline and file sources for one
name refuse startup. Configuration is loaded at startup. To rotate with an
overlap, add a second named token and restart the runtime, move the consumer,
then remove the previous token and restart again. Already admitted work is
not cancelled when a token is removed.

Keep runtime tokens with infrastructure secrets. Never pass them to an agent,
put them in its workspace, or send them to a browser.

## Requests and attribution

Authenticated requests send:

- `Authorization: Bearer <token>`.
- A universe UUID or handle in `/api/v1/universes/{universe}/...` for
  universe resources. The HTTP API rejects `x-lightspeed-universe`.
- Optional `x-lightspeed-actor`, an opaque identifier recorded as the actor.
  Any configured token may name an actor; it grants no permissions.

Records use `{"kind":"actor","id":"alice"}` when an actor was supplied,
otherwise `{"kind":"token","name":"platform"}`. Local and internal
controller attribution remain distinct. `GET /api/v1/connection` reports
runtime identity, contract revision, `single` and, for authenticated calls,
`tokenName`. Single mode also returns `defaultUniverse` for URL selection.

A universe selects a data partition. It does not restrict the token to that
partition. Session list and watch visibility filters are parameters a trusted
gateway supplies; direct runtime access can read private sessions too. Watch
stream limits apply per actor, falling back to token name when no actor is
provided, with a separate total limit.

## Connect the Platform

Set `LIGHTSPEED_API_URL` to the runtime `/api/v1` base URL. The backend reads
`LIGHTSPEED_PLATFORM_API_TOKEN` or `LIGHTSPEED_PLATFORM_API_TOKEN_FILE`; its
secret must match one configured on the runtime. The browser never receives
this token. The Platform supplies the universe and signed-in actor after its
role and visibility checks, including creator-only transcription reads and
cancellation.

For first startup, set `LIGHTSPEED_PLATFORM_ADMIN_EMAIL` and
`LIGHTSPEED_PLATFORM_ADMIN_PASSWORD` in protected configuration. Those values
create the initial local administrator only when the users table is empty.
See [People and roles](people-and-roles.md).

## Connect operators and development clients

The CLI and SDKs can connect directly with a runtime token and an explicit
universe. This grants full deployment authority. See
[Use Lightspeed from the terminal](../using-lightspeed/cli.md) and the
[TypeScript API client](../integrating-and-extending/api-and-typescript.md).

Channel connector startup remains disabled during the transition. Configurator
uses a restricted Platform application key; the development launcher starts it
against Platform without a runtime infrastructure token. Environment daemons,
environment providers and worker-to-environment-gateway credentials are
unchanged; these are separate machine protocols.
