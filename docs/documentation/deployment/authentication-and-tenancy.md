# Authentication and access

The runtime authenticates canonical principals from the [core identity
registry](identity-and-access.md). The Platform signs people in with Better Auth
and currently calls the runtime as its configured service principal. Mapping
Platform users and memberships to core identities is a separate, pending cutover.

## Gateway modes

| Mode | Request identity |
| --- | --- |
| `single` | Explicit local development service principal with deployment and configured-universe administration rights. Startup creates the configured universe and initializes this identity. Rejects bearer, universe and principal headers. Keep this development listener private. |
| `authenticated` | Requires `Authorization: Bearer lsk_…`. Reads the key, active canonical principal and current scoped rights on every request. |

`trusted-header` and `api-key` modes are retired. Schema revision 12 replaces the
API-key table outright; issue canonical credentials after migration. There is no
legacy-key archive or compatibility path. Anonymous principal records are no
longer supported.

A universe key selects exactly its universe. An optional matching
`x-lightspeed-universe` is accepted; a different UUID is rejected. A deployment
key must select a universe with that header for universe/service calls. Neither
scope grants permissions. Deployment administration requires DeploymentAdmin;
service methods require their declared capability, never just service kind.
Key-management methods additionally check issuance authority or key ownership.

`x-lightspeed-principal: user:<canonical-uuid>` (or a bare canonical UUID) is an
assertion, not authentication. Only an authenticated service with `assert_user`
in the target scope may assert an active user. A deployment-scoped credential
may use deployment-wide `assert_user`. The request uses the user's permissions
alone, retaining both authenticated and acting identities in request context.
Missing context and duplicate identity headers fail closed.

**Current boundary:** the gateway and shared services enforce the universe role/action
matrix. Viewer is read-only. Contributors control their own sessions and manage
their own profiles/bots. Operator/Admin can manage bots and profiles, configure
resources, and stop other people's sessions, but cannot steer or delete their
personal sessions. Bot-controlled sessions follow bot-management rights; delegated
children follow explicitly admitted controller lineage. Metadata and provenance
do not grant control. Bot trigger secrets are visible only to managers.

Ownership reservations preserve the creator/controller across retries and deletion.
They are independent of execution credentials. There is no legacy ownership
backfill: content without trusted ownership cannot be claimed by retrying creation.
Mutating service admissions retain actor and credential-reference facts separately
from session content; an admission record does not claim that an operation succeeded.

Session content remains universe-visible. Private-session policies, response-time
reauthorization, and the Platform user-directory cutover remain pending. Committed
status, membership, capability and key changes affect subsequent admission; they
do not stop admitted runs or withdraw an already admitted long-poll response.

## Issue a key for an API client

Run the server CLI from a trusted administrative environment with the runtime
database configured. It does not require Temporal or Platform:

```bash
lightspeed-server migrate
lightspeed-server identity bootstrap \
  --principal-id "<admin-uuid>" --display-name "Administrator"
lightspeed-server universe create --slug acme --creator-principal "<admin-uuid>"
lightspeed-server api-key create --universe-id "<universe-uuid>" \
  --principal "<principal-uuid>" --actor-principal "<issuer-uuid>" --name acme-client
lightspeed-server api-key create --deployment \
  --principal "<service-uuid>" --actor-principal "<admin-uuid>" --name platform
lightspeed-server api-key list
lightspeed-server api-key revoke "<key-prefix>" --actor-principal "<issuer-uuid>"
```

Create users/services and assign roles or capabilities through `identity apply`
or authenticated `deployment/identity/apply`. Both use the same audited core
mutation rules. Universe creation assigns the acting creator as universe Admin.

Members may issue their own universe keys. Universe Admin may issue keys for
services managed in that universe, but cannot impersonate other users or mint
keys for deployment/integration services. DeploymentAdmin may issue deployment
keys and manage principals across the deployment. Key metadata records the bound
principal and issuer separately; plaintext appears only at creation.

## Platform and connectors

Set `LIGHTSPEED_PLATFORM_API_KEY` to an explicitly provisioned Platform service
key and `LIGHTSPEED_API_URL` to the authenticated runtime `/rpc` endpoint. The
Platform sends this key only to the configured runtime URL; per-universe endpoint
overrides cannot receive a credential for another endpoint. The service needs the roles for the operations it performs; existing Platform login
and membership checks remain in place. Better Auth IDs are not core principal
IDs. The key-creation UI therefore asks for an explicit canonical principal ID.

Bootstrap Platform login with `LIGHTSPEED_PLATFORM_ADMIN_EMAIL` and
`LIGHTSPEED_PLATFORM_ADMIN_PASSWORD` while its users table is empty. These
variables do not reset existing passwords. Platform membership removal and core
membership removal remain separate until the directory cutover: removing only
the Platform membership does not revoke a canonical user's direct runtime key.

Connectors use their own `LIGHTSPEED_CONNECTOR_API_KEY`. Assign deployment
`discover_channel_accounts`, and per-universe `lease_credentials` and
`admit_channel_inbound` capabilities as needed. Do not substitute a forged
service principal header. Connectors receive neither `assert_user` nor general
administration by default.

## Configurator and development

Configurator uses `authenticated` mode and forwards its bearer credential and
optional universe/user assertion to the runtime for validation. Its host/origin
checks complement authentication. The Platform setup creates a universe-managed
Configurator service with an Operator assignment and a universe-scoped key,
then stores that key in an outbound auth grant. The old loopback trusted-header
path is retired.

`./dev.sh full` defaults to authenticated mode. When no Platform service key is
configured, it explicitly initializes the local development principal and mints
a launcher key, passing the secret to child processes in memory. `runtime`
defaults to single mode. `identity development --universe-id <uuid>` is a
host-only development bootstrap, never an authenticated gateway fallback.

See [multitenancy](multi-tenancy.md), [self-hosting](self-hosting.md), and the
[environment reference](../reference/environment-variables.md) for deployment
and service configuration.
