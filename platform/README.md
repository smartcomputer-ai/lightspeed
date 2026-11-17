# Lightspeed platform

The first-party Lightspeed management plane, web UI, connector host, and
supporting TypeScript packages. The repository root is the npm workspace root;
run Node commands from there.

## Components

- `backend/` — Hono API, better-auth integration, universe-scoped gateway
  passthrough, database migration startup, and static SPA hosting.
- `web/` — Vite/React management UI served under `/app`.
- `shared/` — Zod input schemas and deterministic helpers shared by the server,
  web UI, and generated clients. The Rust `lightspeed` CLI consumes the native
  Platform route catalog alongside the runtime contract.
- `db/` — Drizzle schemas, migrations, and the platform database adapter.
- `connectors/` — the connector host: one process serving every enabled
  Telegram and WhatsApp account across universes, discovered through the core
  API, with grant-leased provider tokens and one Temporal activity worker per
  account queue. Run it separately as a trusted runtime client. Bot and
  channel workflows run in the Rust runtime.
- `configurator-mcp/` — generated Streamable HTTP MCP facade over the
  universe-scoped Lightspeed API. The Platform setup and development process
  use restricted Platform service-user keys, with the development process
  targeting the Platform shared API.
- `scripts/` — product-identity check and the generated profile configuration
  reference.
- `web/src/demo/` — the in-browser demo backend: an in-memory stand-in for
  the platform backend and engine that the demo build loads instead of a
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
explained in [How access works](../docs/documentation/access-and-security/overview.md).

The SDK [transcript metadata helpers and session-view data guide](../docs/documentation/integrating-and-extending/api-and-typescript.md#build-transcript-authors-and-times)
describe author/time derivation, materialized list fields and watch integration.
The helpers are available independently of the current UI presentation.

The [history loader](web/src/components/session/history-loader.tsx) starts
with recent events, while the [live reader](web/src/lib/sessions/tail.ts)
follows from that window's head. Older pages load as the reader scrolls up.
[Transcript reconstruction](web/src/lib/sessions/transcript.ts) merges those
streams by event and entry ID; historical lifecycle events must not overwrite
live session controls. Changing sessions cancels both read paths. Transient
read failures retain existing content while retrying, and authorization or
integrity errors remain visible immediately.

[Submission reconciliation](web/src/lib/sessions/submissions.ts) combines local
sends with event history in one projection. Local render keys survive confirmation;
input stays visible while the event tail catches up with HTTP receipts. Steering
matches submission and steering IDs and renders within its run from the start.
Local messages appear immediately; incoming messages retain entrance motion.

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
Platform backend, connector host, Configurator MCP, and development-only
settings.

The browser uses `/u/:handle` for universe work and `/admin` for organization
and deployment administration. An Owner enables **Bots** and **Channels** in
the universe's **Settings → General → Features**. The Platform gate enforces
these switches for member requests. Admitted work can continue after a
switch changes.

## People, universes and access

Platform manages accounts, organizations, memberships, and API keys. See
[Organizations, members, and roles](../docs/documentation/access-and-security/people-and-roles.md)
for administrative procedures, [Company sign-in](../docs/documentation/access-and-security/single-sign-on.md)
for identity setup, and [API keys and runtime tokens](../docs/documentation/access-and-security/api-keys-and-service-access.md)
for integration credentials. For local identity development, use the
[Keycloak fixture](../docs/documentation/development/local-development.md#company-sign-in-with-local-keycloak).

The action catalog in [`shared/src/access.ts`](shared/src/access.ts) groups
permissions by deployment, organization, and universe. A caller receives the
combined permissions of their roles at each scope. An API key can narrow
those permissions. The universe and organization views return the resulting
actions for the web UI, and the backend enforces them.

[`backend/src/runtime-client.ts`](backend/src/runtime-client.ts) applies the
[generated operation actions](backend/src/routes/method-actions.ts), feature
switches, and resource-specific checks before forwarding a member request.
The forwarded call uses the Platform runtime token and the member's actor
ID. See [Changing contracts](../docs/documentation/development/changing-contracts.md)
for how to update and verify that metadata.

The Platform backend reads its runtime credential from
`LIGHTSPEED_PLATFORM_API_TOKEN` or `LIGHTSPEED_PLATFORM_API_TOKEN_FILE`.
Configure the same secret as one of the runtime's full-authority tokens.
In local development, Configurator targets Platform using a restricted
service-user key, and the standalone connector host uses a direct runtime
token. See the [environment-variable reference](../docs/documentation/reference/environment-variables.md)
for settings.

## HTTP surfaces

`/api/v1` is the shared runtime resource API. Hono registrations come from the
SDK's generated HTTP catalog, with a reviewed operation admission registry.
Browser and CLI sessions and Platform API keys supply identity. The backend
authorizes universe access, features, and session visibility before calling
the runtime. The shared API exposes admitted universe operations and native
Platform routes. Trusted services use the private runtime API for operations
such as connector ingress and credential leases.

Most request bodies stream unchanged to the runtime, which owns wire validation.
Session creation and run overrides have bounded policy inspection; inspected
JSON rejects unsafe integer values before rewriting. Responses, blobs and named
SSE events stream with cancellation and backpressure. Runtime contract revision
admission prevents forwarding to a mismatched deployment. Native universe
creation, handle changes and deletion coordinate Platform records with runtime
state rather than bypassing the existing lifecycle services.

`/api/ui` contains browser composition and management adapters. The web watch
uses `/api/v1/universes/{universe}/events` directly. Better Auth retains
`/api/auth`; login discovery retains `/api/login-config`. The demo follows the
same URL separation and named event stream format.

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

The focused profile starts shared infrastructure, the Platform backend on port
3000, and Vite on port 5173.

### Demo build

The web UI has two build paths. `npm run build:web` produces the live SPA that
the Platform backend hosts under `/app`. `npm run build:demo` produces
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

To reach states the scripted fixtures do not, a chat message (or steering)
that starts with `debug:` runs a debug command instead of the scripted reply.
The commands come with debug mode (`VITE_ENABLE_DEBUG_MODE=true`, see
`web/src/devtools/debug-mode.ts`); without it their code is not loaded and
such messages get the scripted reply. They live in
`web/src/demo/debug-commands.ts`, and `debug: help` lists them:

| Command | What it does |
| --- | --- |
| `debug: long think` | Thinks for 15 seconds, then answers. |
| `debug: approval` | Asks to approve an MCP tool call; the session waits for approval, and the decision picks the reply. |
| `debug: stop <failed\|timed out\|terminated\|missing>` | Stops the session's workflow that way; Restart clears it. |
| `debug: close` | Closes the session after the reply. |
| `debug: queue <n>` | Queues n follow-up runs behind this one (default 3). |
| `debug: subagents <n>` | Delegates to n sub-agents, which work and finish one after another (default 3). |
| `debug: tools` | Calls one tool of each kind, one of them failing. |
| `debug: error` | Fails the run with a provider error. |
| `debug: long reply` | Answers with a long Markdown reply (lists, a table, code). |
| `debug: env <ready\|provisioning\|paused\|suspended\|stopped\|away\|failed\|unknown>` | Sets the status of the session's active environment. |
| `debug: mcp <auth\|ok>` | Marks the session's MCP server as needing authentication, or active again. |
| `debug: bot <idle\|working\|waiting\|paused\|attention\|budget\|closed>` | In a bot's chat: puts the bot in that state (`waiting` asks for an approval). |
| `debug: bot events <n>` | In a bot's chat: adds n events waiting for the bot (default 3). |
| `debug: bot failed` | In a bot's chat: makes the bot's latest event fail. |

Pages read some of these on their own refresh (environments every 30
seconds), so a change can take a moment to show.

Development defaults use `admin@lightspeed.dev` and
`lightspeed-dev-password`. Override them with
`LIGHTSPEED_PLATFORM_ADMIN_EMAIL` and `LIGHTSPEED_PLATFORM_ADMIN_PASSWORD`.
These defaults are local-only and must never be used in a deployed environment.
The default full launcher also seeds a Test universe with Operator, Contributor,
and Viewer logins using the same initial password; see
[development logins](../docs/documentation/development/local-development.md#development-logins).

Common server settings are listed below. The [Platform variable reference](../docs/documentation/reference/environment-variables.md#platform-backend) includes defaults and deployment-level access settings.

- `LIGHTSPEED_PLATFORM_DATABASE_URL`;
- `LIGHTSPEED_PLATFORM_AUTH_SECRET`;
- `LIGHTSPEED_PLATFORM_API_TOKEN` or `LIGHTSPEED_PLATFORM_API_TOKEN_FILE`;
- `LIGHTSPEED_PLATFORM_BASE_URL`;
- `LIGHTSPEED_PLATFORM_TRUSTED_ORIGINS`;
- `LIGHTSPEED_PLATFORM_ADMIN_EMAIL` and
  `LIGHTSPEED_PLATFORM_ADMIN_PASSWORD`;
- `LIGHTSPEED_PLATFORM_GITHUB_CLIENT_ID` and
  `LIGHTSPEED_PLATFORM_GITHUB_CLIENT_SECRET`;
- `LIGHTSPEED_PLATFORM_WORKSPACE_UPLOAD_MAX_BYTES` and
  `LIGHTSPEED_PLATFORM_WORKSPACE_UPLOAD_MAX_ENTRIES`; and
- `LIGHTSPEED_PLATFORM_CHANNELS_HEALTH_URLS`.

Live database tests require explicit opt-in variables and are not part of the
ordinary unit-test run. Never point a local connector host at production
channel accounts.

CI runs the migration boundary explicitly. To reproduce it:

```bash
LIGHTSPEED_PLATFORM_MIGRATION_TEST_URL=postgres://... npm run test:migrations
```

Release construction stages one platform runtime and one connector-host
runtime (published as `platform-workers`). An operator configures its runtime
token and Temporal connection separately. The release manifest records one
digest for each image.

### Testing & Visual Tests

With `./dev.sh full` running and the Rust CLI built, exercise real Platform
authentication, runtime forwarding, key restrictions, organization/service-user
authority, CLI browser approval, and declarative universe creation:

```bash
source scripts/dev/env.sh
live_connections=$(mktemp -d)
LIGHTSPEED_PLATFORM_LIVE_URL=http://localhost:5173 \
LIGHTSPEED_LIVE_CONNECTIONS_DIR="$live_connections" \
LIGHTSPEED_PLATFORM_LIVE_RENEWAL_DATABASE_URL="$LIGHTSPEED_PLATFORM_DATABASE_URL" \
  npm run test:runtime --workspace @lightspeed-ai/platform-backend
(cd clients/python && LIGHTSPEED_TEST_CONNECTION_CONFIG="$live_connections" \
  uv run pytest tests/live_platform_connections.py -v)
```

These opt-in checks use the seeded admin and operator logins and create
`live-*` organizations and universes in the development database. Set
`LIGHTSPEED_PLATFORM_LIVE_PASSWORD` when the seed password differs from the
development default. The first check saves dedicated CLI/API-key connections
in the private directory; the second exercises both SDKs and revokes those
credentials. Test organizations and universes remain available for inspection.
The renewal database option ages only the dedicated CLI session so the SDK
check can prove real session renewal and credential-file persistence.

The web UI's tests run in Vitest and are part of `npm test` and `npm run check`.
Logic tests call page helpers with plain data; component tests mount pages in
jsdom with the API mocked, click through them, and assert the resulting DOM and
API calls. They sit next to the code they cover (`*.test.ts`, `*.test.tsx`):

```bash
npm run test --workspace @lightspeed-ai/platform-web
npm run test --workspace @lightspeed-ai/platform-web -- src/pages/AccountPage.test.tsx
```

Visual tests capture the demo build in a real browser with Playwright
(`web/e2e/`) and compare two versions with
[screendiff](https://github.com/dankrusi/screendiff). Each screen is captured
in light and dark at a 1440×900 viewport and 2× pixel density:

```bash
npx playwright install chromium               # once
npm run screenshots:demo                      # capture the working tree
npm run screendiff:demo                       # working tree vs. where the branch left main
npm run screendiff:demo -- main~3 HEAD        # any two refs
npm run screendiff:demo -- --theme dark       # one theme
```

Results go to `platform/web/test-results/`: screenshots, and one
self-contained `report.html` per comparison. The report embeds every image, so
a comparison removes its captured PNGs afterwards; pass `--keep-screenshots` to
keep them. A committed ref is built in a temporary worktree and its build is
cached by commit.

Visual tests are not part of the automatic test process and should not be added
to `npm test`, `npm run check`, or CI. Use them as a review tool: to see what a
change does to the UI and UX, and to show it to others before merging. A
changed screen is a prompt to look, not a failure.

### Debug tools

Development builds serve a component catalogue at `/debug/ui-overview`: every
component under `web/src/components/ui` with its variants, grouped by category,
in light and dark. It needs no sign-in, and production builds leave it out.

Debug mode adds tools for reviewing the UI. Enable it in
`platform/web/.env.local` (git-ignored), then restart the dev server:

```bash
VITE_ENABLE_DEBUG_MODE=true
```

Press Ctrl+D to show or hide the debug toolbar in the bottom-right corner:

- **Feedback**: click any element to get a copyable report that identifies it
  (page, component slots, text, React components).
- **Design**: edit the design tokens in a separate window with live preview,
  then export the changes as CSS for `web/src/index.css`.

The tools live in `web/src/devtools/`. The folder is not called `debug` because
the root `.gitignore` ignores that name, which would also hide it from Tailwind.
