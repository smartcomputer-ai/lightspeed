# API keys and runtime tokens

Use a Platform API key to connect a script or integration. A **personal API
key** acts as you. A **service-user API key** acts as an identity the
organization manages independently of the people who run the integration.
Trusted infrastructure connecting directly to the runtime uses a
[runtime token](#configure-runtime-tokens).

## Create a Platform API key

For a personal key, open the account menu and choose **Settings → API keys →
New key**. For an integration, an Organization Admin can set up a service
user:

1. Open **Admin → organization → Members → New service user**. Use the
   **Member** organization role when the integration only needs existing
   universes.
2. In each universe the integration needs, go to **Settings → Members** and
   grant the service user a role.
3. Return to the service user's details under the organization's **Members**
   and choose **New key**. Copy the secret when it appears. Lightspeed shows
   it only once.

Personal organizations are hidden from their owner's Admin sidebar. To manage
a service user there, use the [Platform CLI](../using-lightspeed/cli.md#platform-administration)
or API, or work with a Platform Admin.

In the web app, a personal key defaults to one organization. Choose **All
my organizations** to allow broader access. A service-user key is confined to
its own organization. Select specific **Universes**, switch on **Read only**,
or use **Restrict operations** to limit the key further. The caller must have
permission for the operation and satisfy every key restriction. A key limited
to existing universes cannot create a universe.

Send the key as `Authorization: Bearer <key>` to the Platform's `/api/v1`
base URL. For API examples, see [Platform
access](../integrating-and-extending/api-and-typescript.md#platform-access). If
you are using the CLI as yourself, `lightspeed connect login` signs in through
your browser. See [CLI connections](../using-lightspeed/cli.md#connect-to-platform).

## Review, replace, or revoke a key

Open a key to see its restrictions, expiry, and latest use. An Organization
Admin can review personal and service-user keys that can reach the organization
under **Admin → organization → API keys**. They can revoke service-user keys.
People manage their own personal keys. Credential management requires a human
browser or CLI session.

Keys expire after 365 days by default. Select **No expiry** where the deployment
allows it. For the deployment settings, see the [environment-variable
reference](../reference/environment-variables.md#platform-backend).

- To replace a lost or exposed secret, open the key and choose **Replace
  secret**. The key keeps its ID, restrictions, and expiry. The old secret
  stops working immediately.
- To rotate with overlap, create a second key, move consumers to it, and then
  revoke the old key.
- To cut off a service user, **Disable** it from its details under
  **Admin → organization → Members**. Its keys stop working until it is enabled.

You can also declare service-user keys alongside an organization, with secrets
supplied from your secret store. See [Configure organizations and
access](../deployment/configuration-as-code.md#configure-organizations-and-access).

Personal keys follow their owner's current access. Company users must renew
their admission through [company sign-in](single-sign-on.md#session-expiry-and-suspension).
Service-user keys and local personal keys have their own expiry and revocation.

## Gateway modes

Set `LIGHTSPEED_AUTH_MODE` on the runtime. Use `authenticated` for Platform
and other trusted clients, with at least one configured runtime token. Use
`single` for local development with `LIGHTSPEED_PG_UNIVERSE_ID`. Single mode
accepts only requests without authorization or actor headers.

Every runtime token can call every operation, select any universe, and
access private sessions. Keep the runtime application listener private to
trusted components and operators. Public callbacks, bot webhooks, and
environment connections have separate listeners. See [Public URLs and
internal routes](../deployment/configuration.md#distinguish-public-urls-from-internal-routes).

## Configure runtime tokens

Generate a secret with `lightspeed token generate-secret` and mount it into
the runtime:

```text
LIGHTSPEED_AUTH_MODE=authenticated
LIGHTSPEED_API_TOKEN_FILE=/run/secrets/runtime-token
```

To distinguish consumers in records, use separately named tokens such as
`LIGHTSPEED_API_TOKEN_PLATFORM_FILE` and
`LIGHTSPEED_API_TOKEN_OPERATOR_FILE`. Each token has the same deployment-wide
authority. The [runtime API token
reference](../reference/environment-variables.md#runtime-api-tokens) lists
the accepted settings.

For the Platform backend, set `LIGHTSPEED_API_URL` to the runtime's `/api/v1`
base URL and set `LIGHTSPEED_PLATFORM_API_TOKEN_FILE` to the file containing
one of the runtime's configured secrets. Keep runtime tokens out of browsers,
agent tools, workspaces, and execution environments.

For a trusted client calling the runtime directly, send
`Authorization: Bearer <token>` and use a universe UUID or handle in
`/api/v1/universes/{universe}/…` for universe operations. An optional
`x-lightspeed-actor` header labels who requested the operation. The token
supplies all authority. See [Authenticated runtime from the
CLI](../using-lightspeed/cli.md#configure-an-authenticated-runtime-without-platform).

To rotate with both tokens valid during the cutover, add a second named token
and restart the runtime. Move the client to the new token, then remove the
previous token and restart the runtime again.
