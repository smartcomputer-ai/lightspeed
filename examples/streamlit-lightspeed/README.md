# Lightspeed internals in Streamlit

Chat with a Lightspeed session and follow a turn through the SDK, gateway,
Temporal, worker, model provider, and stores. Click a component to inspect its
evidence. Turn off **Follow latest** to select an earlier run and step through
its timeline.

## Run against the local dev stack

1. From the repository root, run `./dev.sh` and leave it running. In the
   [local Platform](http://127.0.0.1:5173/app/), choose the **Test** universe.
   Connect a model provider and set **Models → Defaults → Agent runs**.
   See the [local setup guide](../../docs/documentation/development/local-development.md).
2. In the universe's **API keys** page, create a key with **session**,
   **vfs**, and **environments** access. The app uses the gateway key from
   its server-side environment. Leave `LIGHTSPEED_UNIVERSE` blank with a
   universe key.
3. Use Python 3.12 or newer and a current [uv](https://docs.astral.sh/uv/).
   From this directory, copy the example environment file, set
   `LIGHTSPEED_API_KEY`, and start Streamlit.

   ```sh
   cp .env.example .env
   uv run --locked streamlit run app.py
   ```

Open [the local demo](http://127.0.0.1:8501). Send a prompt, then a follow-up
in the same session. The current session ID in the sidebar can reopen that
conversation after a browser refresh. Chat works with just the gateway and a
working model.

## Follow a turn across storage and compute

1. Create a workspace in the Test universe and add `notes.txt` with a line
   of sample text. Create a profile that uses the universe model default.
   Enable VFS and mount the workspace at `/workspace` with **Read only**
   access. The [workspace walkthrough](../../docs/documentation/getting-started/first-agent.md)
   shows the profile and attachment controls.
2. In **Environments → Attach local daemon**, register the launcher’s
   loopback daemon at `ws://127.0.0.1:19091/`. Attach it in the same profile
   with **Run commands** access and make it the default environment.
   The [direct attachment guide](../../docs/documentation/environments/bring-your-own-compute.md#direct-attachment-for-local-development)
   explains the permissions of the local daemon.
3. Set `LIGHTSPEED_PROFILE_ID` in the example’s `.env`, restart Streamlit,
   and choose **New session**. Ask the agent to read `/workspace/notes.txt`
   with the VFS tools and run `pwd` in the active environment. Inspect the
   **VFS** and **Daemon / machine** nodes. The workspace file is stored in a
   snapshot. The directory printed by `pwd` belongs to the daemon machine.
4. Click **Temporal** to see session workflow activities. The sample
   environment file points to the launcher’s default address and namespace.
   Copy the displayed workflow ID into Temporal UI for the complete history.
   Select **PostgreSQL** to compare recent durable event sequence numbers with
   the worker events after enabling database evidence below.

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
