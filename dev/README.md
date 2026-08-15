# Lightspeed Development Environment

The top-level `dev/` directory owns the complete local development environment
for Lightspeed: its Docker Compose topology, environment exports, lifecycle
commands, and reset helpers for Postgres, pgAdmin, MinIO, and Temporal.

See [`docs/variables.md`](../docs/variables.md#local-development) for the full
development override table and the separate production component variables.

## Services

- Postgres on `localhost:15432`, hosting separate `lightspeed` runtime and
  `lightspeed_platform` product-plane databases
- pgAdmin on `http://localhost:15080`
- MinIO S3-compatible API on `http://localhost:29000`
- MinIO Console on `http://localhost:29001`
- Temporal on `http://localhost:7233`
- Temporal UI on `http://localhost:8233`

## Unified supervisor

Install the root npm workspace once, then start the complete editable product:

```bash
npm install
npm run dev
```

The supervisor keeps stateful dependencies in Docker and runs editable Rust
and TypeScript processes on the host. It supports four profiles:

```bash
npm run dev -- full       # default: complete product, without credentialed connectors
npm run dev -- platform   # Platform API/UI with the stub Lightspeed gateway
npm run dev -- runtime    # migrated Rust runtime only
npm run dev -- infra      # Postgres, pgAdmin, MinIO, and Temporal only
```

The local UI is available at `http://127.0.0.1:5173/app/`. The supervisor
trusts both `http://127.0.0.1:5173` and `http://localhost:5173` for Better Auth;
additional browser origins must be listed explicitly in
`LIGHTSPEED_PLATFORM_TRUSTED_ORIGINS`.

The `full` profile defaults the runtime to `trusted-header` authentication
because Platform authenticates users and routes every engine request to an
explicit universe. The focused `runtime` profile defaults to `single` for
direct CLI development. An explicit `LIGHTSPEED_AUTH_MODE` overrides either
profile default.

The full profile also runs Configurator MCP plus the Channels workflow and
activity workers. Connectors are opt-in and fail before startup when their
required credentials are missing:

```bash
LIGHTSPEED_CHANNELS_CONNECTORS=telegram npm run dev
LIGHTSPEED_CHANNELS_CONNECTORS=telegram,whatsapp npm run dev
```

Use `npm run dev -- --plan full` to inspect a profile without starting
anything. Pressing Ctrl-C stops host processes but leaves the Docker
infrastructure available. Manage that infrastructure through the same entry
point:

```bash
npm run dev -- status
npm run dev -- down
npm run dev -- down --volumes
npm run dev -- reset
```

## Infrastructure primitives

The shell helpers remain available for live Rust tests and low-level recovery.
Start only the shared Docker infrastructure with:

```bash
dev/up.sh
```

Stop it with:

```bash
dev/down.sh
```

To also remove volumes:

```bash
dev/down.sh -v
```

Reset both databases, apply the ledgered runtime schema, and clear the MinIO
prefix:

```bash
dev/reset.sh
```

Individual helpers:

```bash
dev/pg-reset.sh
dev/pg-migrate.sh
dev/minio-ensure.sh
dev/minio-reset.sh
```

Run the `store-pg` live integration tests against this stack:

```bash
source dev/env.sh
cargo test -p store-pg --test store_pg_live -- --ignored
```

## Runtime Environment

Export local settings into the current shell:

```bash
source dev/env.sh
```

Equivalent values:

```bash
export LIGHTSPEED_TEST_POSTGRES_URL=postgres://lightspeed:lightspeed@localhost:15432/lightspeed
export LIGHTSPEED_PG_UNIVERSE_ID=00000000-0000-0000-0000-000000000001
export LIGHTSPEED_POSTGRES_URL=${LIGHTSPEED_TEST_POSTGRES_URL}
export LIGHTSPEED_PLATFORM_DATABASE_URL=postgres://lightspeed:lightspeed@localhost:15432/lightspeed_platform
export LIGHTSPEED_TASK_QUEUE=lightspeed-agent
export LIGHTSPEED_API_URL=http://127.0.0.1:18080/rpc

export LIGHTSPEED_OBJECT_STORE_BUCKET=lightspeed-dev
export LIGHTSPEED_OBJECT_STORE_ENDPOINT=http://localhost:29000
export LIGHTSPEED_OBJECT_STORE_REGION=us-east-1
export LIGHTSPEED_OBJECT_STORE_PREFIX=lightspeed
export LIGHTSPEED_OBJECT_STORE_FORCE_PATH_STYLE=true

export AWS_ACCESS_KEY_ID=minioadmin
export AWS_SECRET_ACCESS_KEY=minioadmin
```

## Manual runtime roles

The `runtime` profile is the normal way to run the Temporal-backed hosted
runtime against the development stack:

```bash
npm run dev -- runtime
```

For debugging a specific executable role manually:

```bash
source dev/env.sh
cargo run -p temporal-server -- migrate
cargo run -p temporal-server
```

With no subcommand, the `lightspeed-server` binary runs the JSON-RPC gateway and Temporal
worker in one process. For split-role runs, use two shells:

```bash
source dev/env.sh
cargo run -p temporal-server -- worker
```

```bash
source dev/env.sh
cargo run -p temporal-server -- gateway
```

Then chat through the regular CLI over the gateway transport from another
shell:

```bash
source dev/env.sh
cargo run -p cli -- chat --session session_1 "hello"
```

Use `--new` instead of `--session session_1` to create a fresh session id, or
omit the message to open the interactive TUI.

Run the fake hosted-agent live integration test against the same stack:

```bash
source dev/env.sh
cargo test -p temporal-server --test temporal_live temporal_live_session_start_then_run_start_completes_fake_runs -- --ignored --nocapture
```

Run the minimal live environment control-plane acceptance test. This uses real
Postgres and the real lifecycle reconciler with an in-process provider, so it
does not require Incus:

```bash
source dev/env.sh
cargo test -p temporal-server --test environment_provider_live \
  -- --ignored --test-threads=1 --nocapture
```

Run only the OpenAI-backed hosted-agent live test:

```bash
source dev/env.sh
export OPENAI_API_KEY=...
cargo test -p temporal-server --test temporal_live temporal_live_session_start_then_run_start_completes_openai_run -- --ignored --nocapture
```

Set `LIGHTSPEED_OPENAI_MODEL`, `OPENAI_RESPONSES_MODEL`, or
`OPENAI_LIVE_MODEL` to override the default live-test model.

pgAdmin runs in desktop mode for local dev, so the browser UI does not require
a login.

To register the local database in pgAdmin:

```text
Name:                 Lightspeed Runtime
Host name/address:    postgres
Port:                 5432
Maintenance database: lightspeed
Username:             lightspeed
Password:             lightspeed
```

Register the Platform database with the same settings and use
`lightspeed_platform` as its maintenance database.

Use `postgres` as the host inside pgAdmin because pgAdmin runs in the Docker
network. From the host machine, use `localhost:15432` instead:

```text
postgres://lightspeed:lightspeed@localhost:15432/lightspeed
```
