# Universes, Tenant Isolation, and Gateway Authentication

A Lightspeed deployment can serve many isolated universes through one gateway,
one Temporal worker pool, one PostgreSQL server, and one object store. A
universe is the tenant, project, or workspace boundary used throughout the
runtime. It is not a per-user authorization boundary.

This document describes the current operational contract. The implementation
history and design rationale remain in
[P90](roadmap/p90-multi-tenancy.md).

## Isolation model

The public JSON-RPC methods never accept a universe parameter. The gateway
resolves a universe before dispatch, and every universe-scoped method operates
within that resolved context.

Isolation is enforced below the API boundary as well:

- Universe-owned PostgreSQL records use primary and foreign keys scoped by
  `universe_id`; deployment-level universe and API-key records form the
  authentication-edge exceptions.
- Object-store keys are scoped under `universes/{universe_id}/`.
- Temporal workflow IDs are composed as `{universe_id}/{session_id}` while
  public session IDs remain unchanged.
- Per-universe runtime state is resolved lazily over deployment-shared pools
  and clients.
- Registry reads, sessions, fleets, MCP servers, profiles, environments,
  credentials, and blobs cannot resolve across universes.

All universes share the deployment task queue by default. Deployments sharing
one Temporal namespace should set distinct `LIGHTSPEED_TASK_QUEUE` values.

## Gateway authentication modes

`LIGHTSPEED_AUTH_MODE` selects how the HTTP gateway resolves the universe and
caller principal.

| Mode | Universe resolution | Intended use |
| --- | --- | --- |
| `single` | `LIGHTSPEED_PG_UNIVERSE_ID` | Local development or a deployment dedicated to one universe |
| `trusted-header` | Required `x-lightspeed-universe`; optional `x-lightspeed-principal` | A private runtime behind an authenticating Platform or reverse proxy |
| `api-key` | `Authorization: Bearer lsk_...` | Clients connecting directly with a Lightspeed-issued key |

The modes fail closed:

- `trusted-header` rejects missing or unknown universes. Universes are never
  created implicitly.
- `single` and `api-key` reject caller-supplied Lightspeed tenant headers.
- `api-key` exposes the same response for unknown and revoked credentials.
- OAuth callbacks recover their universe from server-side authorization-flow
  state rather than trusting a request header.

`trusted-header` is a trust boundary, not an authentication mechanism. Its
listener must not be directly reachable by untrusted clients; the upstream
service must authenticate the caller, authorize access, and replace the
Lightspeed headers. The first-party Platform performs that role in the full
development profile.

The resolved principal is retained for audit and ownership metadata where
applicable. The runtime does not implement per-user policy within a universe;
that authorization belongs to Platform or another upstream system.

## Provisioning universes

In `single` mode, the configured universe is ensured during startup. In
`trusted-header` and `api-key` deployments, create universes explicitly with
the operator API or the server administration command.

From a source checkout with the runtime environment configured:

```bash
cargo run -p temporal-server -- universe create --slug acme
cargo run -p temporal-server -- universe list
```

An installed server exposes the equivalent commands through
`lightspeed-server universe ...`.

The operator-scoped JSON-RPC methods are:

- `operator/universes/create`
- `operator/universes/list`
- `operator/universes/read`
- `operator/universes/delete`

Operator methods are available only in `single` and `trusted-header` modes;
they are unavailable in `api-key` mode. The first-party Platform uses this
surface after applying its own user and membership authorization.

Deleting a universe terminates its live session workflows, removes its
external blobs, deletes its database records through cascades, and evicts its
cached runtime state.

## API keys

Create an API key only after its universe exists:

```bash
cargo run -p temporal-server -- api-key create \
  --universe-id <uuid> --name acme-production
cargo run -p temporal-server -- api-key list
cargo run -p temporal-server -- api-key revoke <key-prefix>
```

The create command prints the plaintext `lsk_...` secret exactly once. The
database stores its hash and a display prefix, not the recoverable secret.
Clients can provide a key through `LIGHTSPEED_API_KEY`.

The operator API also exposes universe-scoped
`operator/api-keys/create|list|revoke`. A management plane must verify that
the caller may administer the requested universe before invoking these
operator methods.

## Configurator MCP and clients

Configurator MCP must use the same auth mode as its upstream Lightspeed
gateway. It forwards trusted headers or bearer credentials per request and
never exposes `operator/*` methods as MCP tools. See the
[Configurator guide](../platform/configurator-mcp/README.md) for its proxy
configuration.

The Lightspeed CLI reads `LIGHTSPEED_API_KEY` in `api-key` mode and
`LIGHTSPEED_UNIVERSE` in trusted-header development flows. See
[the environment variable reference](variables.md) for the complete runtime,
Platform, Configurator, and client configuration.

## Security boundaries and non-goals

- A universe isolates tenant data; it does not isolate individual users inside
  that universe.
- API keys authenticate a universe and principal but do not add per-resource
  authorization within it.
- Secrets use one deployment-level master key today.
- CAS data is not deduplicated across universes.
- Per-universe Temporal namespaces, task queues, quotas, and rate limits are
  not provided automatically.
