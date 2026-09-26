# Lightspeed platform

The first-party Lightspeed management plane, web UI, connector host, and
supporting TypeScript packages. The repository root is the npm workspace root;
run Node commands from there.

## Components

- `server/` — Hono API, better-auth integration, universe-scoped gateway
  passthrough, database migration startup, and static SPA hosting.
- `web/` — Vite/React management UI served under `/app`.
- `cli/` — `lightspeed-platform`, the platform administration CLI.
- `shared/` — Zod input schemas and deterministic helpers shared by the server,
  web UI, and CLI.
- `db/` — Drizzle schemas, migrations, and the platform database adapter.
- `connectors/` — the connector host: one process serving every enabled
  Telegram and WhatsApp account across universes, discovered through the core
  API, with grant-leased provider tokens and one Temporal activity worker per
  account queue. Bot and channel workflows run in the Rust runtime.
- `configurator-mcp/` — generated Streamable HTTP MCP facade over the
  universe-scoped Lightspeed API.
- `scripts/` — product-identity check and the generated profile configuration
  reference.
- `web/src/demo/` — the in-browser demo backend: an in-memory stand-in for
  the platform server and engine that the demo build loads instead of a
  real API (see "Demo build" below).

The generated public API client lives separately at `clients/typescript/`.
Committed wire artifacts are owned by `crates/api/contract/`.
The repository-level Docker Compose development environment lives under
`scripts/dev/`.

The [session configuration editor](web/src/components/session/session-config-editor.tsx)
controls workspace, environment, and MCP attachments. Access is set per
resource; an MCP selection can narrow the registered server's allowance.
Environment attachments also hold a working directory and default selection.
Profiles can inherit the parent's active environment for sub-agents. See
[Profiles and instructions](../docs/documentation/using-lightspeed/profiles-and-instructions.md)
for how saved setup reaches ordinary sessions and bots.

Transcripts preserve each call's original `toolName` and optional admitted
`toolId`; they do not remap historical calls to today's model-facing names.
[Demo fixtures](web/src/demo/) follow the same rule. Message and reasoning
views carry their projected text, while detailed run reads retain `output`
and `outputText` after the message leaves active context. Tool previews load
bounded excerpts and expand through `blobs/read`.

Input `origin` is display metadata, not authorization. The Platform derives
`user:<id>` from the signed-in user for messages and steering, and ignores
body-supplied origin claims. Bot deliveries use `event`; other API clients
may supply their own strings. Authorization and requester attribution are
explained in [Access and security](../docs/documentation/access-and-security/overview.md).

The [history loader](web/src/components/session/history-loader.tsx) starts
with recent events, while the [live reader](web/src/lib/sessions/tail.ts)
follows from that window's head. Older pages load as the reader scrolls up.
[Transcript reconstruction](web/src/lib/sessions/transcript.ts) merges those
streams by event and entry ID; historical lifecycle events must not overwrite
live session controls. Changing sessions cancels both read paths. Transient
read failures retain existing content while retrying, and authorization or
integrity errors remain visible immediately.

[Run sections](web/src/lib/sessions/run-sections.ts) select the final answer
from the run's recorded output reference, so loading older history cannot
promote an interim message to the final answer. Partially loaded runs must
not present partial usage as a whole-run total. Context statistics describe
the last model request; cumulative usage describes the run. Missing provider
measurements remain unavailable. The
[transcript view](web/src/components/session/transcript-view.tsx) owns the
presentation and its tests; see [Sessions and runs](../docs/documentation/using-lightspeed/sessions-and-runs.md)
for the user controls.

The authoritative configuration reference is
[environment-variable reference](../docs/documentation/reference/environment-variables.md), with separate sections for the
Platform server, connector host, Configurator MCP, and development-only
settings.

The universe sidebar groups the work (Bots, Sessions), the **Setup** agents are
made from (Profiles, Workspaces, Models, Environments, MCP servers), **Access**
(Credentials, API keys, Members) and the universe's **Settings** (General,
Channels, Templates) at flat `/u/:slug/...` routes; bare `/u/:slug/settings`
opens the first settings page the caller may see. Models holds model provider keys,
compatible endpoints and coding-agent subscriptions. Credentials lists reusable
tokens, environment secrets, GitHub App installations and custom OAuth grants;
model and MCP server logins stay on their own pages, while credential pickers
still offer every grant. Credentials, Channels and Templates are for Operators
and Admins; API keys and General are for Admins.

**Features.** A universe admin switches parts of the product on or off under
General → Features; today Bots and Channels, where Channels need Bots. The
registry is `FEATURES` in `platform/shared`, and a universe stores only the
switches set away from their default (`universes.features`). The universe list
carries each feature's effective state, and the web hides a switched-off
feature's pages, menu items and choices. Switching off hides; it does not
enforce: the API stays open, and what already runs, such as a bot answering on
its channels, keeps running.

## People, universes and access

The Platform owns people; core knows universes, keys and opaque actors, and
records who asked for each run, steer, cancellation and approval.

**Accounts.** The browser signs people in with email and password; public
sign-up is closed. The server can configure GitHub OAuth through Better Auth,
but the current sign-in page has no GitHub button. The first platform admin
comes from `LIGHTSPEED_PLATFORM_ADMIN_EMAIL` and
`LIGHTSPEED_PLATFORM_ADMIN_PASSWORD`. Platform admins (the Better Auth `admin`
role) create accounts under **Platform admin → Users**, where they also change a user's
name, verified email, platform-admin role and password; a password reset
revokes the user's sessions. Signed-in users change their own name and
password under **Account**. Self-service email changes wait for an
email-verification sender.

**Universes and roles.** A universe is an organization. Its members hold one of
four roles, least to most:

| Role | May |
| --- | --- |
| viewer | read shared work and their own private sessions |
| contributor | also start and continue sessions and runs, invoke bots, share their own sessions |
| operator | also configure profiles, bots, environments, MCP servers, credentials and channels |
| admin | also manage members and keys, and read, share and delete any session |

A universe's creator is its admin, and a universe always keeps one. A platform
admin acts as an admin in every universe. Universe admins manage members on
**Members**; every member sees names and roles, and admins also see emails.
The organization plugin's own endpoints are not served: membership changes only
through the universe routes.

**The gate.** The server decides before a request reaches core. Every core call
a route makes for a member goes through one client
(`server/src/runtime-client.ts`), which:

- requires the member's role to meet the method's role, from
  `server/src/routes/method-roles.ts`. That table is generated from the core
  method manifest by `node platform/scripts/generate-method-roles.mjs`, and
  `npm run check` fails when it is stale;
- for a method that names a session, unless the member is an admin, requires the
  session to be shared with the universe or created by the member; sharing and
  deleting need its creator;
- narrows a member's session list to shared work and their own; and
- calls core with the Platform's deployment key, naming the universe and the
  member as the actor.

The Platform's own refusals are 403 and 404. Core refusing the Platform is a
server fault (500 or 502), since the member was already admitted. The web's
permission hints come from the same role and never replace these checks.

**Private work.** Sessions start private: their creator and the universe's
admins see them. **Share with universe…** in the session's ⋯ menu shares a
session and its sub-agents, once and for good. The session header marks private
work with a lock and shared work with people; lists mark only shared sessions.
Bots and their conversations are always shared.

**Keys.** `LIGHTSPEED_PLATFORM_API_KEY` is the Platform's deployment key, with
every method group and permission to assert actors. Follow
[Bootstrap the Platform](../docs/documentation/access-and-security/api-keys-and-service-access.md#bootstrap-the-platform)
to create it.
The runtime must run in `authenticated` mode, and connector hosts use their own
`LIGHTSPEED_CONNECTOR_API_KEY`. Universe admins mint keys for their universe
under **API keys**, choosing the method groups each key may call from presets
(agent client, configuration, all groups) or one by one; they never assert
actors, and credential leasing and channel delivery are opt-in. Platform admins see and
mint every key under **Platform admin → API keys**, choosing what a key reaches (the
deployment or one universe), the method groups it may call, and whether it
speaks for people. A secret is shown once; keys never change, so revoke and mint
instead. The Configurator template asks which key its MCP server acts with:
the current one, a new key with chosen groups (starting from the configuration
groups), or an existing universe key whose secret the Admin pastes; it revokes
only keys it minted. The server lists only the tools that key may call, and
anyone who can attach it acts with that key. Operators see the template, and
only Admins install it.

## Development

The manual's [Local development](../docs/documentation/development/local-development.md)
guide covers edit loops across Rust and TypeScript.
[Changing contracts](../docs/documentation/development/changing-contracts.md)
explains API generation and the separately owned Platform migrations.

Install all Node workspace dependencies and run the complete check:

```bash
npm install
npm run check
```

For the complete interactive Lightspeed development stack:

```bash
./dev.sh
```

That command uses the unified supervisor under `scripts/dev/` and starts the complete
product. For the focused Platform loop against an already running runtime at
`LIGHTSPEED_API_URL`, use:

```bash
./dev.sh platform
```

The focused profile starts shared infrastructure, the Platform server on port
3000, and Vite on port 5173.

### Demo build

The web UI has two build paths. `npm run build:web` produces the live SPA that
the Platform server hosts under `/app`. `npm run build:demo` produces
`platform/web/dist-demo/`: the same SPA with `web/src/demo/main.ts` as its
entry, which installs an in-browser backend (a Hono router behind a `fetch`
shim, seeded from `web/src/demo/fixtures/`) before loading the app. It needs no
server, no sign-in, and no network — the visitor is a platform admin who owns a
few pre-populated universes, each showing a different use-case — so it can be
published as a static site (serve `dist-demo/` under `/demo/` with an
`index.html` fallback for client routes).

```bash
./dev.sh demo         # Vite dev server on http://localhost:5175/demo/ (alias: npm run demo)
npm run build:demo    # static site in platform/web/dist-demo/
```

The demo is also the frontend-only development loop: it is the only mock
backend in the repository, so a new API route needs a stub under
`web/src/demo/routes/` (an unstubbed route answers 404 with a `demo:` message
so the gap is visible in the UI) and demo content belongs in a fixture module.

Development defaults use `admin@lightspeed.dev` and
`lightspeed-dev-password`. Override them with
`LIGHTSPEED_PLATFORM_ADMIN_EMAIL` and `LIGHTSPEED_PLATFORM_ADMIN_PASSWORD`.
These defaults are local-only and must never be used in a deployed environment.
The default full launcher also seeds a Test universe with Operator, Contributor,
and Viewer logins using the same initial password; see
[development logins](../docs/documentation/development/local-development.md#development-logins).

The server accepts the following primary configuration names:

- `LIGHTSPEED_PLATFORM_DATABASE_URL`;
- `LIGHTSPEED_PLATFORM_AUTH_SECRET`;
- `LIGHTSPEED_PLATFORM_BASE_URL`;
- `LIGHTSPEED_PLATFORM_TRUSTED_ORIGINS`;
- `LIGHTSPEED_PLATFORM_ADMIN_EMAIL` and
  `LIGHTSPEED_PLATFORM_ADMIN_PASSWORD`;
- `LIGHTSPEED_PLATFORM_GITHUB_CLIENT_ID` and
  `LIGHTSPEED_PLATFORM_GITHUB_CLIENT_SECRET`;
- `LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_URL` and the optional
  `LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_ALLOW_PRIVATE_NETWORK`; and
- `LIGHTSPEED_PLATFORM_CHANNELS_HEALTH_URLS`.

Imported pre-release aliases were removed as part of the greenfield
product-identity reset. Platform deployments must use the
`LIGHTSPEED_PLATFORM_*` names above.

Live database tests require explicit opt-in variables and are not part of the
ordinary unit-test run. Never point a local connector host at production
channel accounts.

CI runs the migration boundary explicitly. To reproduce it:

```bash
LIGHTSPEED_PLATFORM_MIGRATION_TEST_URL=postgres://... npm run test:migrations
```

Release construction stages one platform runtime and one connector-host
runtime (still published as the `platform-workers` image so image references
and manifest keys stay stable). The connector host serves the providers named
by `LIGHTSPEED_CONNECTOR_PROVIDERS` for every account the core reports; see
`connectors/README.md`. The release manifest records one digest for each
image.
