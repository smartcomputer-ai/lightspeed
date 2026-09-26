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
  account queue. It replaces the former `bots/`, `channels/`, and `workers/`
  packages; Bots and Channels core now live in the Rust runtime.
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

Specific tool choices in session and profile configuration use registry IDs
(for example, `env.run_process`). The runtime chooses builtin names and schemas
for each model. Transcripts preserve each call's original `toolName` alongside
its optional admitted `toolId`; the UI does not resolve historical names again.
Demo tool fixtures record these identities explicitly as well.

Session and profile editors configure workspace and environment attachments
with access per resource. Environment rows also carry a default selection and
working directory; saved profiles can inherit a parent environment for
sub-agents. MCP server allowances and session subsets share a searchable
tool picker with live descriptions and connection errors. Session subsets
can only narrow the server's allowance. Session activation controls offer attached environments
and remain separate from profile defaults. Session creation uses the default
environment attachment in the effective config, without a separate override.

Context views and run outputs share a content descriptor. Assistant messages,
reasoning, and audio transcripts can reference JSON; API views include their
full projected text. Detailed run reads include `output` and `outputText` even
after the message leaves active context. The transcript renders messages and
reasoning directly. Tool previews stay bounded and expand their original bytes
through `blobs/read`.

Conversation input items accept an optional `origin` string (1–200 bytes, not
blank). The message and steering routes derive `user:<id>` from the authenticated
platform session; request bodies cannot override it. Bot deliveries, their media,
and batch framing use `event`, including steering and context-only delivery.
Other API clients may supply other origin strings. Origin is display metadata,
not an authorization claim, and absent origin means unknown. It is persisted on
each input/context entry and returned by context, event, and detailed run reads.
All user-role inputs use the standard muted message palette, regardless of
origin, including optimistic sends and steering. The composer uses the standard
background, muted placeholder, and visible focus ring. Origin adds no visible
label or color emphasis. All user-role messages collapse to
about 206px when their rendered text exceeds 160px, with a bottom fade and
keyboard-accessible Show more / Show less controls. Resizing rechecks wrapping,
and expansion leaves bottom-following to keep the message in view.
Locally submitted run inputs keep the same message element and expansion state
through confirmation, whether acknowledgement arrives through the POST or event
stream first. Submitted messages, steering, and queued inputs use their normal
opacity immediately; acceptance causes no visual change. Failed sends remove
the optimistic message and show an error in the composer.
Backend-loaded runs use run IDs as rendering keys. Loading an older input or
submission acknowledgement does not remount the run or reset its expanded state.
New inputs, activity boxes, and assistant messages enter with a 200 ms fade and
4 px rise. Initial history and older-page arrivals stay still; confirmation,
text updates, final-answer promotion, and opening hidden activity do not replay
entrances. Reduced-motion preferences disable them, and layout height is not animated.
Messages arriving while their activity box is entering share that animation;
only messages arriving after it ends get an individual entrance. Thinking traces
and tool batches do not animate separately inside the box.

Session transcripts open with one recent event window from
`session/events/read` with `direction: "backward"`, then follow `session/events/read` strictly after
that initial window's head. Scrolling near the top automatically requests
older windows through an independent exclusive `before` cursor. There is no
history cutoff or load-more button. The message scroller preserves the visible
message when older entries are prepended; its loading sentinel sits outside
the content element so it cannot hide prepends from the scroller.

Windows may split a run or tool batch. Results without a loaded call start
render as continued tool activity and acquire their original metadata as older
pages arrive. Partial generation totals are not presented as whole-run usage.
The transcript groups entries into run sections: the input band, the work
(thinking, tool calls, interim notes, steering, context updates), the final
reply, and the outcome. Every step is one row — activity icon, verb, target,
detail, duration — and success carries no badge; running, waiting, failed and
cancelled rows are the only marked ones. Row details (Arguments, Result,
Effects) open beneath the row with a mono meta line: tool name, call id, start
time, duration, output size. Activity families come from the API's
`ToolCallDisplayGroup` (explore, edit, execute, mcp, agent, bot, message,
other); bot rows use the bot mark, and Emit rows resolve peer bot ids to
display names through the bot roster. Delivered bot events (`origin: "event"`)
render as bands headed by sender, kind and `#N`.
Completed replies are selected by the run's recorded output reference, not by
the last transcript row. Chat Completions reasoning appears before the message
from the same turn; this display ordering does not rewrite stored context or
reorder other providers' native output items. Missing output entries become
visible as their history page loads, without promoting an interim message.
A finished run's work folds behind one strip naming the outcome and duration
("Worked for 2m 14s", "Failed after 38s"), the tool call count and failures.
From medium widths up the strip ends with a hoverable statistics button
showing last-call context and cumulative input-plus-output usage; on narrow
screens that button is the first row of the opened run instead, so the strip
stays readable. A run that did no tool work shows the same figures on its
outcome line. Counts below 1,000 stay exact; larger counts use `k`. Its
popover breaks usage into input, output, model calls, tool calls, and the
cache-hit share; duration lives on the strip, not in the popover. Missing
provider counts remain unavailable rather than becoming zero. Failed and
cancelled runs retain their visible status. The context measurement describes
the last request, not the next request's assembled context or the model's
capacity. Statistics stay in the transcript; the composer has no context
indicator. Durations render as whole milliseconds under 100 ms, tenths of a
second under 10 s, then seconds, minutes and hours. Two preferences live in
the session-title and active bot-conversation menus: "Collapse completed runs"
(default on; applies when a session or older history loads, and a strip click
overrides it per run until the preference changes) and "Show run statistics"
(default on; hides the statistics button and standalone duration lines for
runs without activity, while failures and cancellations stay visible). Both
are saved per user in local storage, shared across sessions, bots, universes,
and tabs in this browser. A live run streams open with a status row at its
foot; a folded run mounts none of its work.
History is reconstructed chronologically and deduplicated by event/entry ID;
historical lifecycle transitions never overwrite live controls. History errors
retry independently of live polling, and changing sessions aborts both paths.
Live polling retries a transient connection failure immediately with `waitMs: 0`
and at most one event from the unchanged cursor. A failed recovery probe starts
a ten-second presentation grace period before a muted disconnect notice;
an empty successful probe clears it immediately and resumes normal long-polling.
Authorization and event-integrity errors remain visible immediately. Live reads
have a deadline ten seconds beyond their requested wait so stalled connections
cannot stop updates indefinitely.
Transcript and list read errors share this presentation rule: transient transport,
timeout, rate-limit, and server errors stay quiet for ten seconds in transcripts
and three seconds in lists, then show a
muted connection notice. Initial reads show loading during that interval; cached
content remains visible. Lists retain one retry for transient failures and expose
actionable errors without a retry delay. Notices only say "retrying" where the
transcript reader continues retrying; mutation error handling is unchanged.

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

**Accounts.** Better Auth signs people in with email and password, and with
GitHub when configured; public sign-up is closed. The first platform admin
comes from `LIGHTSPEED_PLATFORM_ADMIN_EMAIL` and
`LIGHTSPEED_PLATFORM_ADMIN_PASSWORD`. Platform admins (the Better Auth `admin`
role) create accounts under **Admin → Users**, where they also change a user's
name, verified email, platform-admin role and password; a password reset
revokes the user's sessions. Signed-in users change their own name and
password under **Account**. Self-service email changes wait for an
email-verification sender.

**Universes and roles.** A universe is an organization. Its members hold one of
four roles, least to most:

| Role | May |
| --- | --- |
| viewer | read shared work |
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
every method group and allowed to assert actors (`server api-key bootstrap`).
The runtime must run in `authenticated` mode, and connector hosts use their own
`LIGHTSPEED_CONNECTOR_API_KEY`. Universe admins mint keys for their universe
under **API keys** (every universe group, no actor). Platform admins see and
mint every key under **Admin → API keys**, choosing what a key reaches (the
deployment or one universe), the method groups it may call, and whether it
speaks for people. A secret is shown once; keys never change, so revoke and mint
instead. The Configurator template mints its own universe key with the
configuration groups; anyone who can attach its MCP server configures the
universe with that key. Operators see the template, and only Admins install it.

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
