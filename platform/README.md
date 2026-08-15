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
- `scripts/` — platform process orchestration, the stub gateway, and generated
  profile configuration reference.

The generated public API client lives separately at `clients/typescript/`.
Committed wire artifacts are owned by `crates/api/contract/`.
The repository-level Docker Compose development environment lives under
`scripts/dev/`.

The authoritative configuration reference is
[`docs/variables.md`](../docs/variables.md), with separate sections for the
Platform server, Channels, Configurator MCP, and development-only settings.

## Development

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
product. For the focused Platform loop with a stub gateway, use:

```bash
./dev.sh platform
```

The focused profile starts shared infrastructure, the stub gateway, the
Platform server on port 3000, and Vite on port 5173. Set
`LIGHTSPEED_PLATFORM_DEV_REAL_GATEWAY=1` to use an external Lightspeed gateway
at `LIGHTSPEED_API_URL` instead of the stub.

Development defaults use `admin@lightspeed.dev` and
`lightspeed-dev-password`. Override them with
`LIGHTSPEED_PLATFORM_ADMIN_EMAIL` and `LIGHTSPEED_PLATFORM_ADMIN_PASSWORD`.
These defaults are local-only and must never be used in a deployed environment.

The server accepts the following primary configuration names:

- `LIGHTSPEED_PLATFORM_DATABASE_URL`;
- `LIGHTSPEED_PLATFORM_AUTH_SECRET`;
- `LIGHTSPEED_PLATFORM_BASE_URL`;
- `LIGHTSPEED_PLATFORM_TRUSTED_ORIGINS`;
- `LIGHTSPEED_PLATFORM_ADMIN_EMAIL` and
  `LIGHTSPEED_PLATFORM_ADMIN_PASSWORD`;
- `LIGHTSPEED_PLATFORM_GITHUB_CLIENT_ID` and
  `LIGHTSPEED_PLATFORM_GITHUB_CLIENT_SECRET`;
- `LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_URL`; and
- `LIGHTSPEED_PLATFORM_CHANNELS_HEALTH_URLS`.

Imported pre-release aliases were removed as part of the greenfield
product-identity reset. Platform deployments must use the
`LIGHTSPEED_PLATFORM_*` names above.

Live database or Temporal integration tests require explicit opt-in variables
and are not part of the ordinary unit-test run. Never use production connector
credentials for local Telegram or WhatsApp workers.

CI runs those integration boundaries explicitly. To reproduce them:

```bash
LIGHTSPEED_PLATFORM_MIGRATION_TEST_URL=postgres://... npm run test:migrations
npm run test:integration:channels
```

Release construction stages one platform runtime and one Channels runtime.
The Channels image includes every role and connector dependency and is started
as `workflows`, `activities`, `telegram`, `whatsapp`, or `all`. The P123
manifest records one digest for each image. Foundry has no independent release
image.
