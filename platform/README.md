# ls.bot app

The ls.bot control plane (see [docs/platform-plan.md](../docs/platform-plan.md)):
DB-backed platform config on the `lsbot` database, user auth and universe
membership via better-auth, and a small UI. Universes start blank and the app
creates them explicitly through `operator/universes/create`; profiles and
workspaces are edited directly through the in-app gateway passthrough.

npm-workspaces monorepo, TypeScript end to end, run with tsx (no build step):

- `db/` — Drizzle schema + SQL migrations for the `lsbot` database.
  better-auth tables are generated from `db/scripts/auth-codegen.ts`; after
  regenerating, run `node db/scripts/patch-auth-schema.mjs` (the generator
  emits `timestamp` without timezone; we store instants, so every column is
  patched to `timestamptz`), then `npm run generate -w db` for the SQL
  migration. Platform tables live in `src/schema/platform.ts`.
- `core/` — shared input schemas (zod) and small domain helpers.
- `server/` — Hono API + better-auth (organization, admin, bearer plugins),
  applies migrations on startup, serves `/health`, `/api/auth/*`, `/api/v1/*`
  and the `/app` SPA. Includes the universe-scoped gateway passthrough
  (`src/routes/gateway.ts`): profiles list/read/put/delete and workspaces
  list/create against `LIGHTSPEED_API_URL` (trusted-header mode), owner/admin
  gated — pure passthrough, no platform-side mirror of engine state.
  Universe lifecycle is explicit: create calls `operator/universes/create`
  (engine auto-create is retired), permanent delete purges via
  `operator/universes/delete` (archived universes only, platform admin).
- `web/` — the frontend: Vite + React SPA served by the server under `/app`
  (react-router, react-query, better-auth React client; Tailwind v4 +
  shadcn/Base UI, components vendored in `src/components/ui/`). App shell
  with universe switcher; per-universe routes: Profiles (as-JSON editor)
  and Workspaces at the top level, configuration under
  `/app/u/:slug/settings/{general,setups,environments,mcp-servers,channels,api-keys,members}`. Platform admin
  area (`/app/admin/{users,universes,channels}`), account page with password
  change, dark mode. See `docs/ui-plan.md`. `npm run build:web`
  produces `web/dist`; for UI development run `npm run dev -w web` (vite on
  :5173, proxying `/api` to the server on :3000).
- `cli/` — the `lsbot` operator CLI (`cli/bin/lsbot`), which talks to the API
  with a bearer token from `lsbot login`.
- `channels/` — Temporal workflows and account-affine Telegram/WhatsApp
  connectors. Each channel route owns a managed Lightspeed session; pushed
  workflow tools schedule provider activities directly and resolve on
  provider acknowledgement. There is no delivery outbox or message ledger.
  Postgres contains only channel accounts, bindings, pairings, identities,
  and access data. The package's `@lightspeed/agent-client` dependency remains
  a `file:` link into the sibling `../lightspeed` checkout until published.
  Connector health ports also serve Prometheus admission/availability metrics
  at `/metrics`; Temporal SDK metrics are exposed by workflow, activity, and
  provider roles on their configured `CHANNELS_METRICS_PORT`.

## Development

Principle: **containers for state, host processes for code you edit.**

```bash
npm install
npm run dev     # db (compose postgres) + stub gateway + api (tsx watch) + web (vite HMR)
```

`scripts/dev-stack.mjs` brings up everything for the inner loop:

- **api** on :3000 — tsx watch, restarts on save; applies migrations and
  seeds the admin (`lukas@smartcomputer.company` / `dev-password`, override
  via `LSBOT_ADMIN_*`).
- **web** on :5173/app/ — vite dev server with HMR, proxying `/api` to the
  api. Use this URL for UI work; :3000/app serves the last `build:web`.
- **stub gateway** on :19999 — in-memory Lightspeed (profiles incl.
  list/update-patch/delete + VFS workspaces + sessions with a canned chat
  reply ~1s after `session/runs/start`) for developing the gateway
  passthrough without the Rust stack. `GET /calls` shows traffic. It is a
  plain node process: after editing it, restart the stack. Deliberately
  minimal — anything deeper gets tested against the real engine
  (`LSBOT_DEV_REAL_GATEWAY=1` below).
- **db** — the compose postgres (real roles + init scripts, prod parity).
  Password resolves from `infra/.env`; the development stack uses `:15433`
  by default so it can run beside Lightspeed on `:15432`. Override it with
  `POSTGRES_PORT=<port> npm run dev` when needed.

Integration against the real engine: `LSBOT_DEV_REAL_GATEWAY=1 npm run dev`
points the gateway URL at :18080 — run lightspeed via `infra/scripts/up`
(full compose) or `cargo run` from the sibling checkout. Run Channels roles
with `npm run dev:workflows -w @lightspeed/channels`, `dev:activities`,
`dev:telegram`, or `dev:whatsapp`. Never run a second connector with production
credentials: Telegram polling and WhatsApp linking are single-consumer, so use
a dedicated development bot or number.

DB-gated integration tests: point `LSBOT_TEST_DATABASE_URL` at any scratch
postgres and `npm test`.

Signup is disabled; accounts beyond the seeded admin are created via
`lsbot user create` or the Users page.

Deployment: `infra/docker/lsbot-app.Dockerfile`, compose service `app`,
routed by Caddy under `ls.bot/api` and `ls.bot/app`.
