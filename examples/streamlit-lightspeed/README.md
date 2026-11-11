# Lightspeed internals in Streamlit

Chat with a Lightspeed session and follow a turn through the SDK, gateway,
Temporal, worker, model provider, and stores. Click a component to inspect its
evidence. Turn off **Follow latest** to select an earlier run and step through
its timeline.

## Configure Lightspeed

Start `./dev.sh` from the repository root and wait for the gateway, Platform,
and daemon to become ready. From this example's directory, run:

```sh
./init-env.sh --universe test
```

The initializer connects to the running Lightspeed gateway and provisions the
demo resources in the selected universe. It uses the local launcher’s
deployment key when the gateway matches the launcher handoff. For another
deployment, supply `LIGHTSPEED_DEMO_ADMIN_API_KEY` in your shell.

| Lightspeed resource | What setup does |
| --- | --- |
| Universe | Uses an existing universe. `--universe` accepts a UUID or slug. Without it, setup uses the app connection's universe or defaults to the local Platform **Test** universe. |
| Model | Uses the profile model or the universe's **Agent runs** default. The model options below configure a route specifically for the demo profile. |
| Workspace | Creates `demo-workspace` with a `notes.txt` fixture when the workspace is missing. |
| Environment | Attaches the launcher daemon at `ws://127.0.0.1:19091/`. For another gateway, pass `--environment-id` for an existing environment or `--envd-endpoint` for a reachable daemon. |
| Profile | Creates `internals-demo` with a read-only workspace mounted at `/workspace` and a default command-capable environment. Use `--profile` to choose another profile ID. |
| API key | Reuses a usable universe key from the example configuration or provisions a scoped key with `session`, `vfs`, and `environments` access. A new key is saved to the owner-only file printed by the command. |

For a different deployment, specify its gateway and target universe:

```sh
./init-env.sh --gateway https://runtime.example.com/rpc \
  --universe "$DEMO_UNIVERSE_ID" \
  --environment-id "$DEMO_ENVIRONMENT_ID"
```

### Supply a model key

Copy the bootstrap template to the gitignored, owner-only file
`.env.bootstrap` in this directory:

```sh
install -m 600 .env.bootstrap.example .env.bootstrap
```

Set `LIGHTSPEED_DEMO_MODEL_API_KEY` in that file and run
`./init-env.sh --universe test`. For an OpenRouter key, setup configures
DeepSeek V4 Flash through OpenRouter when the demo profile needs a model.
Other keys use the provider selected by the demo profile or the universe's
**Agent runs** default. You can also put `LIGHTSPEED_DEMO_ADMIN_API_KEY` here
when provisioning a different gateway. Exported variables override the file.

To give the demo profile its own model route, pass all three model options:

```sh
./init-env.sh --universe test --model-provider openai \
  --model-api-kind openai:responses --model "$DEMO_MODEL"
```

Setup preserves existing credentials and universe model defaults. Rotate an
existing provider key in Lightspeed. Repeating setup reuses the workspace,
profile, and key. Streamlit reads its connection settings separately from
`.env`.

## Connect Streamlit

With Python 3.12+ and a current [uv](https://docs.astral.sh/uv/), copy
`.env.example` to `.env` for a fresh app configuration. Set `LIGHTSPEED_API_URL`
to the gateway, `LIGHTSPEED_API_KEY` to the scoped key, and `LIGHTSPEED_PROFILE_ID`
to the profile from setup. A universe key selects its universe, so leave
`LIGHTSPEED_UNIVERSE` blank. Use `LIGHTSPEED_DEMO_PLATFORM_URL` for the
**Open Lightspeed** link.

The example file includes the local Temporal address, namespace, and UI.
Set `LIGHTSPEED_DEMO_POSTGRES_DSN` using the [read-only grants below](#enable-read-only-postgresql-evidence)
to include PostgreSQL evidence. Then start Streamlit:

```sh
uv run --locked streamlit run app.py
```

Open the [Streamlit demo](http://127.0.0.1:8501) and send this prompt:

> Read `/workspace/notes.txt` and run `pwd` in the active environment.

Click **VFS** to preview the file, **Daemon / machine** to inspect the command,
**Temporal** for workflow activity, and **PostgreSQL** to compare durable event
sequence numbers. The workspace file lives in a snapshot. The directory printed
by `pwd` belongs to the daemon machine.

## Read the map

Teal arrows mark observed HTTP requests and replies. Dashed amber arrows mark
routes inferred from the selected run’s recorded events or the gateway
operation. Gray arrows show the topology. Bold arrows mark the selected step.
Temporal history and infrastructure panels include session-level evidence.
The example’s own inspection requests are outside the turn timeline.

## Enable read-only PostgreSQL evidence

Use the runtime database of your local dev stack. Connect as a local database
administrator, for example with the default Compose service:

```sh
docker exec -it lightspeed-postgres psql -U lightspeed -d lightspeed
```

Create a dedicated login and grant access to the metadata columns the demo reads.

```sql
CREATE ROLE lightspeed_demo_readonly LOGIN;
\password lightspeed_demo_readonly
GRANT USAGE ON SCHEMA public TO lightspeed_demo_readonly;
GRANT SELECT (universe_id, session_id, lifecycle_status, head_seq, created_at_ms, updated_at_ms)
    ON public.sessions TO lightspeed_demo_readonly;
GRANT SELECT (universe_id, session_id, seq, event_kind, observed_at_ms)
    ON public.session_events TO lightspeed_demo_readonly;
GRANT SELECT (universe_id, session_id, through_seq, format_version, byte_len, created_at_ms)
    ON public.session_checkpoints TO lightspeed_demo_readonly;
```

Set `LIGHTSPEED_DEMO_POSTGRES_DSN` in the example’s `.env` using that login
and the local PostgreSQL port, `15432` by default. Restart the app.
The Temporal and PostgreSQL connections are intended for a private local
deployment. Each view limits the metadata it loads and supports manual refresh.

Run the offline demo tests from this directory with `uv run --locked pytest`.
