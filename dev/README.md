# Forge Local Dev

This directory contains a local Docker environment for the PostgreSQL session/CAS store.

## Services

- Postgres on `localhost:15432`
- pgAdmin on `http://localhost:15080`
- MinIO S3-compatible API on `http://localhost:29000`
- MinIO Console on `http://localhost:29001`
- Temporal on `http://localhost:7233`
- Temporal UI on `http://localhost:8233`

## Start

```bash
dev/local/up.sh
```

## Stop

```bash
dev/local/down.sh
```

To also remove volumes:

```bash
dev/local/down.sh -v
```

## Reset

Reset the database, apply the `store-pg` schema, and clear the MinIO prefix:

```bash
dev/local/reset.sh
```

Individual helpers:

```bash
dev/local/pg-reset.sh
dev/local/pg-migrate.sh
dev/local/minio-ensure.sh
dev/local/minio-reset.sh
```

Run the `store-pg` live integration tests against this stack:

```bash
source dev/local/env.sh
cargo test -p store-pg --test store_pg_live -- --ignored
```

## Runtime Environment

Export local settings into the current shell:

```bash
source dev/local/env.sh
```

Equivalent values:

```bash
export FORGE_TEST_POSTGRES_URL=postgres://forge:forge@localhost:15432/forge
export FORGE_PG_UNIVERSE_ID=00000000-0000-0000-0000-000000000001
export FORGE_POSTGRES_URL=${FORGE_TEST_POSTGRES_URL}
export FORGE_TASK_QUEUE=forge-agent
export FORGE_LLM=fake
export FORGE_API_URL=http://127.0.0.1:18080/rpc

export FORGE_OBJECT_STORE_BUCKET=forge-dev
export FORGE_OBJECT_STORE_ENDPOINT=http://localhost:29000
export FORGE_OBJECT_STORE_REGION=us-east-1
export FORGE_OBJECT_STORE_PREFIX=forge
export FORGE_OBJECT_STORE_FORCE_PATH_STYLE=true

export AWS_ACCESS_KEY_ID=minioadmin
export AWS_SECRET_ACCESS_KEY=minioadmin
```

## Temporal Worker

Run the Temporal-backed agent loop against the local stack:

```bash
source dev/local/env.sh
cargo run -p worker
```

Optionally run the JSON-RPC gateway in a second shell:

```bash
source dev/local/env.sh
cargo run -p gateway
```

Then chat through the regular CLI over the gateway transport from another
shell:

```bash
source dev/local/env.sh
cargo run -p cli -- chat --session session_1 "hello"
```

Use `--new` instead of `--session session_1` to create a fresh session id, or
omit the message to open the interactive TUI.

Run the fake hosted-agent live integration test against the same stack:

```bash
source dev/local/env.sh
cargo test -p gateway --test temporal_live temporal_live_session_start_then_run_start_completes_fake_runs -- --ignored --nocapture
```

Run only the OpenAI-backed hosted-agent live test:

```bash
source dev/local/env.sh
export OPENAI_API_KEY=...
cargo test -p gateway --test temporal_live temporal_live_session_start_then_run_start_completes_openai_run -- --ignored --nocapture
```

Set `FORGE_OPENAI_MODEL`, `OPENAI_RESPONSES_MODEL`, or
`OPENAI_LIVE_MODEL` to override the default live-test model.

pgAdmin runs in desktop mode for local dev, so the browser UI does not require
a login.

To register the local database in pgAdmin:

```text
Name:                 Forge Local
Host name/address:    postgres
Port:                 5432
Maintenance database: forge
Username:             forge
Password:             forge
```

Use `postgres` as the host inside pgAdmin because pgAdmin runs in the Docker
network. From the host machine, use `localhost:15432` instead:

```text
postgres://forge:forge@localhost:15432/forge
```
