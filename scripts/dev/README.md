# Lightspeed Development Environment

The root `dev.sh` launcher and its implementation under `scripts/dev/` own the
complete local development environment for Lightspeed: first-run checks,
dependency bootstrap, Docker Compose topology, environment exports, lifecycle
commands, and reset helpers for Postgres, pgAdmin, MinIO, and Temporal.

See [`docs/variables.md`](../../docs/variables.md#local-development) for the full
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

From a fresh checkout, start the complete editable product with one command:

```bash
./dev.sh
```

The launcher checks Node, Cargo, Docker, and Docker Compose. For profiles that
run TypeScript, it installs the root npm workspace when dependencies are
missing or `package-lock.json` changed. A root `.env` is loaded automatically;
when no common model-provider credential is present, startup continues with an
actionable warning. Copy `.env.example` to `.env` to configure provider keys.

`npm run dev` delegates to the same root launcher, so these are equivalent:

```bash
./dev.sh platform
npm run dev -- platform
```

The supervisor keeps stateful dependencies in Docker and runs editable Rust
and TypeScript processes on the host. It supports four profiles:

```bash
./dev.sh full       # default: complete product, without credentialed connectors
./dev.sh platform   # Platform API/UI with the stub Lightspeed gateway
./dev.sh runtime    # migrated Rust runtime only
./dev.sh infra      # Postgres, pgAdmin, MinIO, and Temporal only
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
LIGHTSPEED_CHANNELS_CONNECTORS=telegram ./dev.sh
LIGHTSPEED_CHANNELS_CONNECTORS=telegram,whatsapp ./dev.sh
```

Use `./dev.sh --plan full` to inspect a profile without starting
anything. Pressing Ctrl-C or running `stop` from another terminal stops the
tracked host supervisor and its children while leaving Docker infrastructure
available. `down` performs a complete teardown in the safe order: host
processes first, then infrastructure.

```bash
./dev.sh status
./dev.sh stop
./dev.sh down
./dev.sh down --volumes
./dev.sh reset
```

`status` reports both the host supervisor and Compose services. The supervisor
stores its local process metadata under the ignored `.lightspeed/` directory.
`reset` refuses to recreate databases while the supervisor is running; stop it
first.

## Infrastructure primitives

The supported developer entry point is always `./dev.sh`. Small shell
primitives remain under `scripts/dev/infra/` so live Rust tests and low-level
recovery do not depend on the product supervisor. They are internal
implementation details rather than a second command surface.

Start only the shared Docker infrastructure through the public command:

```bash
./dev.sh infra
```

The corresponding low-level primitives are:

```bash
scripts/dev/infra/up.sh
scripts/dev/infra/down.sh [--volumes]
scripts/dev/infra/reset.sh
scripts/dev/infra/pg-reset.sh
scripts/dev/infra/pg-migrate.sh
scripts/dev/infra/minio-ensure.sh
scripts/dev/infra/minio-reset.sh
```

`reset.sh` recreates both databases, applies the runtime's ledgered schema, and
clears the Lightspeed MinIO prefix. Platform applies its independently owned
database migrations when the Platform server starts.

Run the `store-pg` live integration tests against this stack:

```bash
source scripts/dev/env.sh
cargo test -p store-pg --test store_pg_live -- --ignored
```

## Runtime Environment

Export local settings into the current shell:

```bash
source scripts/dev/env.sh
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

The fixed local secret-store key is intentionally public development material.
Its Lightspeed-owned value replaced an imported pre-release key; development
state encrypted with the old key must be reset with `./dev.sh reset`.

## Manual runtime roles

The `runtime` profile is the normal way to run the Temporal-backed hosted
runtime against the development stack:

```bash
./dev.sh runtime
```

For debugging a specific executable role manually:

```bash
source scripts/dev/env.sh
cargo run -p temporal-server -- migrate
cargo run -p temporal-server
```

With no subcommand, the `lightspeed-server` binary runs the JSON-RPC gateway and Temporal
worker in one process. For split-role runs, use two shells:

```bash
source scripts/dev/env.sh
cargo run -p temporal-server -- worker
```

```bash
source scripts/dev/env.sh
cargo run -p temporal-server -- gateway
```

Then chat through the regular CLI over the gateway transport from another
shell:

```bash
source scripts/dev/env.sh
cargo run -p cli -- chat --session session_1 "hello"
```

Use `--new` instead of `--session session_1` to create a fresh session id, or
omit the message to open the interactive TUI.

Run the fake hosted-agent live integration test against the same stack:

```bash
source scripts/dev/env.sh
cargo test -p temporal-server --test temporal_live temporal_live_session_start_then_run_start_completes_fake_runs -- --ignored --nocapture
```

Run the minimal live environment control-plane acceptance test. This uses real
Postgres and the real lifecycle reconciler with an in-process provider, so it
does not require Incus:

```bash
source scripts/dev/env.sh
cargo test -p temporal-server --test environment_provider_live \
  -- --ignored --test-threads=1 --nocapture
```

Run only the OpenAI-backed hosted-agent live test:

```bash
source scripts/dev/env.sh
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
