# Authentication and access

Lightspeed has two authentication boundaries. The Platform signs people in
and checks their access to a universe. The runtime gateway resolves the
universe and principal for an API request. In a full installation, the
Platform connects these boundaries by calling a private runtime gateway on
behalf of the signed-in user.

Configure that path first, then add accounts and client credentials. The
[multitenancy guide](multi-tenancy.md) explains what universes isolate and
which infrastructure they share. The [self-hosting guide](self-hosting.md)
provides the initial service configuration and public proxy routes.

## Choose the gateway authentication mode

Each gateway process selects one `LIGHTSPEED_AUTH_MODE`:

| Mode | How a universe-scoped request is resolved | Suitable boundary |
| --- | --- | --- |
| `single` | Uses `LIGHTSPEED_PG_UNIVERSE_ID` and the default principal. Startup ensures that universe exists. | A private installation with one universe and trusted callers. |
| `trusted-header` | Requires `x-lightspeed-universe: <uuid>`. An upstream service authenticates the caller and chooses the UUID. | The private gateway behind the Platform or another trusted application. |
| `api-key` | Resolves `Authorization: Bearer lsk_…` to a stored universe and principal. | A gateway for clients using runtime API keys. |

The Platform sends trusted universe headers, so its upstream gateway must use
`trusted-header`. Keep that listener private. The headers contain an identity
assertion; they do not prove that an internet caller is entitled to it.

For universe-scoped requests, both `single` and `api-key` reject caller-supplied
universe and principal headers. In `trusted-header`, a missing or unknown
universe fails rather than creating a tenant. Create universes through the Platform or administration
commands before admitting requests.

An optional `x-lightspeed-principal` header accepts `user:<id>` or
`service_account:<id>`. A bare value is a user ID; omission uses
`universe_default`. The runtime uses principals for attribution and some
service-method checks. They do not introduce per-user access rules for ordinary
resources inside a universe.

### Keep deployment and service access distinct

Deployment methods manage deployment resources, including universes and API
keys. The runtime accepts them through `single` and `trusted-header` gateways
without an additional deployment login. They reject a universe header, and an
`api-key` gateway rejects deployment methods entirely. Network access to a
private deployment-capable listener therefore carries substantial authority.
The Platform checks its own administrator permissions before making these
calls.

Service methods, such as `auth/grants/lease` and `channels/inbound/admit`,
require a `service_account` principal in the two multitenant modes. `single`
mode bypasses that check. Service identity is intended for trusted adapters
such as the connector host; it is separate from a person's Platform role.

## Set up the first administrator

Before the first Platform startup, provide
`LIGHTSPEED_PLATFORM_ADMIN_EMAIL` and `LIGHTSPEED_PLATFORM_ADMIN_PASSWORD`,
along with its database URL, stable authentication secret, and public base
URL. The bootstrap creates an administrator only while the entire users table
is empty. Changing the variables later does not change an existing password.

Sign in at `/app/` and verify that **Admin** is available. After bootstrap,
remove the initial password from the deployment configuration used for future
starts. Keep `LIGHTSPEED_PLATFORM_AUTH_SECRET` stable; it is part of the
Platform's authentication state.

Email/password authentication is enabled, and public password signup is
disabled. Administrators can create accounts directly. Optional GitHub login
uses both `LIGHTSPEED_PLATFORM_GITHUB_CLIENT_ID` and
`LIGHTSPEED_PLATFORM_GITHUB_CLIENT_SECRET`. Enabling a login provider does not
itself assign a user to a universe.

## Create a universe and add people

Use a platform administrator account for this procedure:

1. Choose **New universe** and create the team's universe. The Platform
   creates its runtime universe and records the mapping to the Platform
   organization.
2. Open **Admin → Users → Create user**. Enter the person's name, email, and
   initial password, and choose the platform `user` role unless they should
   administer the entire installation.
3. Select the universe and open **Settings → Members → Add member**.
4. Select the existing **Account**, choose its universe role, and add it.

Adding a member selects an existing account. This flow does not create the
account or send an email invitation.

Platform and universe roles serve different purposes:

| Role | Current access |
| --- | --- |
| Platform administrator | Manages users and all universes, including creation, adoption, and permanent deletion. Does not need membership in each universe. |
| Universe owner or admin | Manages that universe's configuration, memberships, API keys, sessions, profiles, workspaces, and integrations. Can update or archive the universe. |
| Universe member | Can view the bot roster/activity, channel/account listings, and membership. This role does not provide general access to session and setup pages. |

The ordinary **member** role is narrower than a general read-only runtime
account. Use owner/admin for someone following the setup and session
walkthroughs in this manual.

To find the runtime UUID, open **Settings → General → Identifiers →
Lightspeed universe**. It differs from the browser URL's slug and the
Platform's own record ID. Trusted-header clients and server administration
commands use this runtime UUID.

## Issue a key for an API client

In the universe, open **Settings → API keys → Create key**. Enter a name that
identifies the client, then choose **Create key**. Copy the value from
**Copy your API key**, store it with the client, and choose **I saved the key**.
The complete secret is shown once. The runtime retains its hash and a display
prefix, which lets it recognize or revoke the key without recovering the
secret.

The Platform creates the key with the current user's principal. A key grants
ordinary runtime API access within its universe; it has no configurable
expiration or fine-grained resource scopes. Only owners, admins, and platform
administrators can list or manage keys through the Platform.

The client must use an `api-key` gateway endpoint. Creating a key does not
change the authentication mode of the Platform's private gateway. If the
deployment needs both paths, run a separate `gateway` process in `api-key`
mode against the same deployment stores, queues, and environment gateway.
Give it its own listener and HTTPS route. Keep the deployment-capable endpoint
private. [Configuration](configuration.md) and [operations](operations.md)
explain the settings shared by these processes.

For the Lightspeed CLI, set `LIGHTSPEED_API_URL` to that gateway's `/rpc` URL
and supply `LIGHTSPEED_API_KEY` through the client's secret configuration.
Leave `LIGHTSPEED_UNIVERSE` unset for API-key access. The key determines the
universe; callers cannot select another one in a request body or header.

Revoke a key from **Settings → API keys** when its client is retired or the
secret is exposed. Revocation rejects subsequent authenticated requests. It
does not cancel a run or automation already admitted by the runtime.

### Administration without the Platform

The server binary has commands that use deployment storage directly. Run
them with the intended runtime database configuration, from a protected
administrative environment:

```bash
lightspeed-server universe create --slug acme
lightspeed-server universe list
```

Use the returned UUID to create a client key:

```bash
lightspeed-server api-key create \
  --universe-id "<universe-uuid>" --name acme-production
lightspeed-server api-key list
lightspeed-server api-key revoke "<key-prefix>"
```

These commands do not require an HTTP gateway to be running. In a source
checkout, the equivalent prefix is `cargo run -p temporal-server --`. Store
the create command's one-time key output as a secret.

## Remove access deliberately

Removing a Platform membership and revoking a runtime key are separate
operations. Runtime key resolution does not consult the Platform membership
table, so a removed member's previously issued key continues to work until
revoked. Include both operations when someone leaves a team.

The administrator's password-reset flow also revokes the user's Platform
authentication sessions. It does not revoke their runtime keys. Review those
keys separately, together with any work or automation that should stop.

Archiving a universe changes its Platform status and ordinary navigation. It
does not revoke keys, block existing API paths, or stop runtime activity. Use
the [universe lifecycle procedure](multi-tenancy.md#archive-and-delete-a-universe)
when retiring a tenant.

## Connect Configurator MCP

Configurator uses the same authentication mode as its upstream runtime
gateway and forwards the request identity. It does not exchange a Platform
login session for a runtime key. An API-key Configurator therefore needs an
API-key runtime endpoint, even when the Platform uses another private gateway.

Configurator exposes ordinary management tools; deployment and service methods
are excluded. Its HTTP host/origin allowlists are additional request checks,
not a substitute for gateway authentication. See the
[Configurator service guide](../../../platform/configurator-mcp/README.md)
and its [configuration variables](../reference/environment-variables.md#configurator-mcp).

## Verify access

Sign in as a newly configured owner/admin, select the intended universe, and
open a session or profile. Then sign in as a member to check the narrower
navigation. For an API client, make a small request through its API-key
endpoint and verify that a revoked test key is rejected on the next request.
Use disposable test credentials for the revocation check.

| Symptom | What to check |
| --- | --- |
| Sign-in succeeds but setup is denied | The account needs universe owner/admin or platform administrator access. |
| Bootstrap variables do not change a password | They only initialize an empty user table. Reset the existing account through Admin Users. |
| A Platform universe page fails at the runtime | Its upstream must be reachable and use `trusted-header`; verify the runtime UUID mapping. |
| A valid key is rejected | Check the endpoint's mode, revocation status, and absence of tenant/principal headers. |
| A removed member can still use the API | Revoke their runtime keys explicitly. |
| A service method rejects an ordinary key | It requires a service-account principal in a multitenant mode. |
