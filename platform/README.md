# Lightspeed platform

The first-party Lightspeed management plane, web UI, channel workers, and
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
- `channels/` — Temporal-managed Telegram and optional WhatsApp channel roles.
- `configurator-mcp/` — generated Streamable HTTP MCP facade over the
  universe-scoped Lightspeed API.
- `foundry/` — mechanically imported compatibility code. Foundry is not a
  supported P124 release component and receives no new architecture work until
  its product future is decided.
- `scripts/` — the local development stack, stub gateway, and generated profile
  configuration reference.

The generated public API client lives separately at `clients/typescript/`.
Committed wire artifacts are owned by `crates/api/contract/`.

## Development

Install all Node workspace dependencies and run the complete check:

```bash
npm install
npm run check
```

For the interactive platform development stack:

```bash
npm run dev
```

That command starts the repository's local Postgres service when needed, a
stub Lightspeed gateway, the platform server on port 3000, and the Vite web UI
on port 5173. Set `LIGHTSPEED_PLATFORM_DEV_REAL_GATEWAY=1` to use a real
Lightspeed gateway at `LIGHTSPEED_API_URL` instead of the stub.

Development defaults use `admin@lightspeed.dev` and
`lightspeed-dev-password`. Override them with
`LIGHTSPEED_PLATFORM_ADMIN_EMAIL` and `LIGHTSPEED_PLATFORM_ADMIN_PASSWORD`.
These defaults are local-only and must never be used in a deployed environment.

The server accepts the following primary configuration names:

- `LIGHTSPEED_PLATFORM_DATABASE_URL`;
- `LIGHTSPEED_PLATFORM_AUTH_SECRET`;
- `LIGHTSPEED_PLATFORM_BASE_URL`;
- `LIGHTSPEED_PLATFORM_ADMIN_EMAIL` and
  `LIGHTSPEED_PLATFORM_ADMIN_PASSWORD`;
- `LIGHTSPEED_PLATFORM_GITHUB_CLIENT_ID` and
  `LIGHTSPEED_PLATFORM_GITHUB_CLIENT_SECRET`;
- `LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_URL`; and
- `LIGHTSPEED_PLATFORM_CHANNELS_HEALTH_URLS`.

Legacy `LSBOT_*` aliases remain accepted for the production compatibility
window described by P124. Conflicting new and legacy values are rejected.

Live database or Temporal integration tests require explicit opt-in variables
and are not part of the ordinary unit-test run. Never use production connector
credentials for local Telegram or WhatsApp workers.

CI runs those integration boundaries explicitly. To reproduce them:

```bash
LIGHTSPEED_PLATFORM_MIGRATION_TEST_URL=postgres://... npm run test:migrations
npm run test:integration:channels
```

Release construction stages one platform runtime, a standard Channels runtime
without WhatsApp-only dependencies, and a separate WhatsApp-enabled runtime.
The P123 manifest records the platform plus workflow, activity, Telegram, and
optional WhatsApp images by digest. Foundry has no independent release image.
