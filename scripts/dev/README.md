# Lightspeed Development Environment

Start with the manual's
[Local development](../../docs/documentation/development/local-development.md)
guide for setup and edit loops, and
[Testing and evaluation](../../docs/documentation/development/testing-and-evaluation.md)
for choosing checks and running live suites. This file retains launcher,
service, and manual-startup details.

The root `dev.sh` launcher and its implementation under `scripts/dev/` own the
complete local development environment for Lightspeed: first-run checks,
dependency bootstrap, Docker Compose topology, environment exports, lifecycle
commands, and reset helpers for Postgres, pgAdmin, MinIO, and Temporal.

See [environment-variable reference](../../docs/documentation/reference/environment-variables.md#local-development) for the full
development override table and the separate production component variables.

## Services

- Postgres on `localhost:15432`, hosting separate `lightspeed` runtime and
  `lightspeed_platform` product-plane databases
- pgAdmin on `http://localhost:15080`
- MinIO S3-compatible API on `http://localhost:29000`
- MinIO Console on `http://localhost:29001`
- Temporal on `http://localhost:7233`
- Temporal UI on `http://localhost:8233`

The local Temporal namespace is created with the Channels visibility search
attributes (`LightspeedUniverseId`, `LightspeedChannelProvider`,
`LightspeedChannelAccountId`, `LightspeedBotTriggerId`, and `LightspeedBotId`).
Conversation workflows attach these indexes when they start so the Temporal UI
and operational tooling can locate them without reading workflow state.

## Unified supervisor

From a fresh checkout, start the complete editable product with one command:

```bash
./dev.sh
```

The launcher checks Node, Cargo, Docker, and Docker Compose. For profiles that
run TypeScript, it installs the root npm workspace when dependencies are
missing or `package-lock.json` changed. A root `.env` is loaded automatically.
The `full` and `runtime` profiles can start without `OPENAI_API_KEY` or
`ANTHROPIC_API_KEY`. The launcher warns when neither deployment key is set;
in the full product, add a universe-scoped key under **Models → Add provider**.
Provider-backed runs still need a valid credential for the selected model.

To require a deployment key before startup, for example in CI:

```bash
./dev.sh --require-api-keys
./dev.sh runtime --require-api-keys
```

The older `--allow-missing-api-keys` flag is accepted for compatibility and has
no effect. Deployment-wide keys can still be configured in the root `.env`.

`npm run dev` delegates to the same root launcher, so these are equivalent:

```bash
./dev.sh platform
npm run dev -- platform
```

The supervisor keeps stateful dependencies in Docker and runs editable Rust
and TypeScript processes on the host. It supports six profiles:

```bash
./dev.sh full       # default: complete product, without credentialed connectors
./dev.sh platform   # Platform API/UI against the runtime at LIGHTSPEED_API_URL
./dev.sh runtime    # migrated Rust runtime only
./dev.sh demo       # web UI only, over the in-browser demo backend (no Docker)
./dev.sh docs       # documentation with live reload (aliases: doc, documentation)
./dev.sh infra      # Postgres, pgAdmin, MinIO, and Temporal only
```

The docs profile opens the manual at `http://127.0.0.1:4321/docs/` and needs only
Node and npm. Ctrl-C or `./dev.sh stop` stops its server.

The `full` and `runtime` profiles also start a local `lightspeed-envd` by
default, listening on `127.0.0.1:19091` with its working directory under
`.lightspeed-dev/envd/workspace`. Pass `--no-envd` or set
`LIGHTSPEED_DEV_ENVD=off` to disable it. The launcher does not automatically
register or select this environment for sessions. The
[compute walkthrough](../../docs/documentation/environments/bring-your-own-compute.md#direct-attachment-for-local-development)
shows how to attach it. Commands run as the daemon's OS user; its working
directory is not a process sandbox.

The local UI is available at `http://localhost:5173/app/`. The supervisor
trusts both `http://127.0.0.1:5173` and `http://localhost:5173` for Better Auth;
additional browser origins must be listed explicitly in
`LIGHTSPEED_PLATFORM_TRUSTED_ORIGINS`.

The authenticated full profile also ensures a **Test** universe and Admin,
Operator, Contributor, and Viewer logins on each startup. The three additional
emails are `operator@lightspeed.dev`, `contributor@lightspeed.dev`, and
`viewer@lightspeed.dev`; new accounts use the configured Admin password.
Existing passwords and universe content are preserved. Set
`LIGHTSPEED_PLATFORM_DEV_SEED=false` to opt out; see
[development logins](../../docs/documentation/development/local-development.md#development-logins).

The `full` profile defaults to authenticated runtime access. Without a configured
`LIGHTSPEED_PLATFORM_API_KEY`, the launcher bootstraps the configured universe
and mints a deployment key with every method group and actor assertion, passing
the secret to child processes in memory.
The `runtime` profile defaults to `single` for direct CLI development.

Platform waits for the configured runtime's HTTP health endpoint before starting,
including in the `platform` profile. Its universe operations need that runtime,
so Rust compilation must finish first. Platform creates its first local admin
from the configured email and password when its own user table is empty. The
supervisor logs this dependency wait and fails startup after 60 seconds if the
runtime never becomes ready. `--plan` also shows startup dependencies.

The full profile also runs Configurator MCP; Bots and Channels core run inside
the Rust runtime. The connector host is opt-in: naming providers starts one
`connectors` process that discovers every enabled account of those providers
through the core API and leases their tokens from it (WhatsApp additionally
needs `LIGHTSPEED_CONNECTOR_WHATSAPP_MEDIA_LOCATOR_KEY` and pairs by QR code):

```bash
LIGHTSPEED_CHANNELS_CONNECTORS=telegram ./dev.sh
LIGHTSPEED_CHANNELS_CONNECTORS=telegram,whatsapp ./dev.sh
```

Configurator setup provisions a universe key with configuration method groups
and stores it in a credential grant. Enabled connectors require their own
`LIGHTSPEED_CONNECTOR_API_KEY` with the groups needed for account discovery,
inbound admission, credential leasing, and media upload. See
[API keys and service access](../../docs/documentation/access-and-security/api-keys-and-service-access.md)
for the exact groups and request headers.

Use `./dev.sh --plan full` to inspect a profile without starting services.
Planning an enabled local daemon can create its working directory.
Pressing Ctrl-C or running `stop` from another terminal stops the
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

## Startup errors

Failures identify the step and service, explain what was observed, and suggest
the next check. Failed commands include their working directory, exit status,
and recent output. A readiness timeout includes the endpoint and last HTTP or
connection error. Port conflicts are checked before infrastructure or migrations
start.

The launcher stops its host processes after a startup failure and reports any
forced termination. Docker infrastructure stays in place; use `./dev.sh status`
to inspect it and `./dev.sh down` to stop it. Fix the reported problem before
retrying; a migration failure is not a reason to reset your database.

For launcher stack traces, add `--debug` to the same command:

```bash
./dev.sh docs --debug
./dev.sh runtime --debug
```

Service output still streams normally, including any diagnostics printed by
the service itself. Recent output is held in memory for the failure summary,
not written to an additional log file.

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

The runtime's fresh PostgreSQL baseline is organized into eleven domain migrations
in `crates/store-pg/migrations/`: identity and scoped API keys share a definition;
resource ownership and access audit remain separate. Auth and environment
alterations are folded into their owning definitions. Databases from earlier
baselines require a reset of the schema and migration ledger; deleting rows alone is not
enough. The migrator keeps rejecting changed checksums and never resets data
automatically. Use `./dev.sh reset` for disposable local development state.

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
export LIGHTSPEED_TASK_QUEUE=lightspeed-sessions
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

With no flags, the `lightspeed-server` binary runs every role — the JSON-RPC
`gateway`, the `environment-gateway` (worker environment routes, the public
daemon registration routes, and the environment reconcilers), plus the
`sessions`, `bots`, and `channels` Temporal workers, each on its own task
queue — in one process. For split-role runs, select roles per shell
(`--task-types workflows|activities` splits a worker role further); keep the
`environment-gateway` role on exactly one process and point the others at it
through `LIGHTSPEED_ENVIRONMENT_GATEWAY_URL`:

```bash
source scripts/dev/env.sh
cargo run -p temporal-server -- --roles sessions,bots,channels
```

```bash
source scripts/dev/env.sh
cargo run -p temporal-server -- --roles gateway,environment-gateway
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
cargo test -p temporal-server --test sessions_live temporal_live_session_start_then_run_start_completes_fake_runs -- --ignored --test-threads=1 --nocapture
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
cargo test -p temporal-server --test sessions_live temporal_live_session_start_then_run_start_completes_openai_run -- --ignored --test-threads=1 --nocapture
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

The full authenticated development profile runs `server api-key bootstrap` for
the development universe: it creates the universe if needed, revokes the
previous launcher key, and mints a deployment key with every method group that
may assert actors. The launcher passes its secret to the Platform as
`LIGHTSPEED_PLATFORM_API_KEY`; the Platform asserts the signed-in user as the
actor on interactive requests.
The greenfield identity migration refuses a populated old Platform schema;
reset disposable Platform state explicitly instead of importing old permissions.
