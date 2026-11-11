# Lightspeed internals in Streamlit

Chat with a Lightspeed session and follow a turn through the SDK, gateway,
Temporal, worker, model provider, and stores. Click a component to inspect its
evidence. Turn off **Follow latest** to select an earlier run and step through
its timeline.

## Run against the local dev stack

Start `./dev.sh` from the repository root and wait for the gateway, Platform,
and daemon to become ready. Then run the following from this example's
directory with Python 3.12+ and a current [uv](https://docs.astral.sh/uv/):

```sh
./init-env.sh
uv run --locked streamlit run app.py
```

Open the [Streamlit demo](http://127.0.0.1:8501). Choose **Open Lightspeed**
in the sidebar to visit the Platform. To follow a turn through storage and
compute, send this prompt:

> Read `/workspace/notes.txt` and run `pwd` in the active environment.

Click **VFS** to preview the file, **Daemon / machine** to inspect the command,
**Temporal** for workflow activity, and **PostgreSQL** to compare durable event
sequence numbers. The workspace file lives in a snapshot. The directory printed
by `pwd` belongs to the daemon machine.

## What the initializer configures

| Component | Local setup | Existing deployment |
| --- | --- | --- |
| Gateway and key | Uses the launcher handoff to create an isolated runtime universe and writes a scoped app key to `.env`. | Set `LIGHTSPEED_DEMO_ADMIN_API_KEY` in the setup shell. Pass the gateway URL and universe ID. The app key needs `session`, `vfs`, and `environments` access. |
| Model | Starts a deterministic loopback provider with scripted chat and tool calls. | Use a profile with a working model or select one under **Models → Defaults → Agent runs**. |
| VFS and profile | Seeds `notes.txt` in `demo-workspace` and attaches it read-only at `/workspace` in `internals-demo`. | Reuses an existing profile with both attachments. If the profile is absent, creates it using `demo-workspace` and the chosen environment. Put `notes.txt` in the workspace if you reuse one. |
| Environment | Attaches the launcher daemon at `ws://127.0.0.1:19091/` and makes it the default environment for the demo profile. | Pass `--environment-id` for an existing machine or `--envd-endpoint` for a direct endpoint the gateway can reach. The profile needs a default environment with **Run commands** access. |
| Platform | Links to `http://127.0.0.1:5173/app/`. | Pass `--platform-url` or set `LIGHTSPEED_DEMO_PLATFORM_URL` to the web app address. |
| Temporal | Uses the launcher's local frontend, namespace, and UI. | Set `LIGHTSPEED_DEMO_TEMPORAL_ADDRESS`, `LIGHTSPEED_DEMO_TEMPORAL_NAMESPACE`, and `LIGHTSPEED_DEMO_TEMPORAL_UI` if you want workflow evidence. |
| PostgreSQL | Grants a dedicated read-only login access to the metadata columns used by the demo. | Supply `LIGHTSPEED_DEMO_POSTGRES_DSN` with a read-only login. See [the grants below](#enable-read-only-postgresql-evidence). |

Rerun `./init-env.sh` to reuse the same resources and restart its fixture if
needed. The deployment key is used during setup. The protected app environment
file holds the scoped key and reader settings. The initializer leaves existing
model defaults, profiles, and workspace contents in place.

To point at an existing deployment, supply its deployment key as
`LIGHTSPEED_DEMO_ADMIN_API_KEY` in the shell and run:

```sh
./init-env.sh --gateway https://runtime.example.com/rpc \
  --universe "$DEMO_UNIVERSE_ID" \
  --environment-id "$DEMO_ENVIRONMENT_ID" \
  --platform-url https://lightspeed.example.com/app/
```

Choose a universe already managed in Platform if you want to find the same
sessions and resources there. The default local initializer creates a separate
runtime universe. Use `--env-file` to keep a second configuration, then pass
the same file to `uv run --locked --env-file <file> streamlit run app.py`.
Use `--no-local-postgres` to skip provisioning the local reader.

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
