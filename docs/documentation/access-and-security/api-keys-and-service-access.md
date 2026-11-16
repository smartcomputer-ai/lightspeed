# Runtime tokens and service access

The runtime authenticates trusted components with tokens from configuration.
Every runtime token has full authority over every universe and method. People
use the Platform, which checks membership, roles and resource visibility. A
standalone runtime needs its own trusted gateway or a trusted network.

Runtime API keys, key-management methods, method-group permissions and key
pages have been removed. Personal and service-account tokens through the
Platform are separate work; runtime tokens are not their replacement for
untrusted applications or agents.

## Gateway modes

`LIGHTSPEED_AUTH_MODE` is required:

| Mode | Behavior |
| --- | --- |
| `authenticated` | Requires at least one configured RPC token. Invalid credentials receive HTTP 401 before the request body is read. |
| `single` | Unauthenticated local development, using `LIGHTSPEED_PG_UNIVERSE_ID`. Credential, universe and actor headers are rejected. |

The runtime currently serves RPC, OAuth callbacks and bot webhooks on the
same gateway listener. Restrict `/rpc` at your network or reverse-proxy boundary.
A dedicated private RPC listener is planned separately.

## Configure tokens

Use an inline secret or a mounted file:

```text
LIGHTSPEED_AUTH_MODE=authenticated
LIGHTSPEED_RPC_TOKEN_FILE=/run/secrets/runtime-token
```

The plain token is named `default`. Optional names distinguish components in
attribution without changing their authority:

```text
LIGHTSPEED_RPC_TOKEN_PLATFORM_FILE=/run/secrets/platform-token
LIGHTSPEED_RPC_TOKEN_OPERATOR_FILE=/run/secrets/operator-token
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
- `x-lightspeed-universe`, as a UUID or handle, for universe and service methods.
  Omit it for deployment methods and `initialize`.
- Optional `x-lightspeed-actor`, an opaque identifier recorded as the actor.
  Any configured token may name an actor; it grants no permissions.

Records use `{"kind":"actor","id":"alice"}` when an actor was supplied,
otherwise `{"kind":"token","name":"platform"}`. Local and internal
controller attribution remain distinct. `initialize` reports `single` and,
for authenticated calls, `tokenName`.

A universe selects a data partition. It does not restrict the token to that
partition. Session list and watch visibility filters are parameters a trusted
gateway supplies; direct runtime access can read private sessions too. Watch
stream limits apply per actor, falling back to token name when no actor is
provided, with a separate total limit.

## Connect the Platform

Set `LIGHTSPEED_API_URL` to the runtime `/rpc` endpoint. The backend reads
`LIGHTSPEED_PLATFORM_RPC_TOKEN` or `LIGHTSPEED_PLATFORM_RPC_TOKEN_FILE`; its
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

Channel connector startup and Platform Configurator setup are disabled during
this transition. Neither receives a runtime token. Environment daemons,
environment providers and worker-to-environment-gateway credentials are
unchanged; these are separate machine protocols.
