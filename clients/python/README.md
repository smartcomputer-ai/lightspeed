# Lightspeed Python SDK

Use the async client to start, follow, and inspect durable Lightspeed runs.
The Python and TypeScript clients use the same JSON-RPC contract.

## Install from the checkout

Requires Python 3.12 or newer. From the repository root:

```sh
python -m pip install ./clients/python
```

Use a checkout that matches the Lightspeed runtime version you are targeting.
For an example with its own environment and a live data-flow map, see the
[Streamlit demo](../../examples/streamlit-lightspeed).

## Run a task

Set `LIGHTSPEED_API_URL` to the gateway’s `/rpc` endpoint and
`LIGHTSPEED_API_KEY` to a key with session access. A deployment key also needs
a universe UUID. The universe needs an agent model default or a named profile.

```python
import asyncio
import os

from lightspeed_ai import AsyncLightspeedClient


async def main() -> None:
    async with AsyncLightspeedClient(
        os.environ["LIGHTSPEED_API_URL"],
        api_key=os.environ["LIGHTSPEED_API_KEY"],
        universe=os.environ.get("LIGHTSPEED_UNIVERSE") or None,
    ) as client:
        session = await client.start_session(session_id="notes-demo", display_name="Notes")
        session_id = session["session"]["id"]
        started = await client.start_run(
            session_id,
            "Suggest a title for these release notes.",
            submission_id="title-1",
        )
        async with asyncio.timeout(60):
            result = await client.await_run(session_id, started["run"]["id"])
        print(result["run"].get("outputText") or result["run"]["status"])


asyncio.run(main())
```

Named helpers use snake_case arguments and return the method’s data.
Use `call("session/read", {"sessionId": session_id})` to pass exact wire fields
and receive the full outcome, including notifications. Responses use the
gateway’s JSON keys. `lightspeed_ai.types` exports the generated wire types.

Store a session ID and submission ID before sending a mutation so you can
retry with the same IDs and inputs after an ambiguous response. Pass the
last cursor received from the `heartbeat` callback as `after` when
resuming `await_run`. A Python task cancellation or timeout ends observation.
Call `cancel_run` to ask the runtime to cancel the durable run.

`LightspeedRpcError` preserves the gateway’s code, kind, message, and data.
`LightspeedTransportError` carries transport and response failures separately.
Only the reads inside `await_run` retry automatically. An injected
`httpx.AsyncClient` stays owned by its caller.

## Develop and test

Run from `clients/python` with current uv, Python 3.12+, and Node available
for reading the shared TypeScript helper inventory.

```sh
uv sync --locked
uv run --locked python scripts/generate.py --check
uv run --locked pytest
uv run --locked mypy
```

Omit `--check` to regenerate the Python bindings after exporting the API.
For the opt-in gateway test, build the TypeScript SDK with
`npm run build --workspace @lightspeed-ai/sdk` from the repository root.
Then point the usual environment variables at a local gateway with a
working model and run:

```sh
LIGHTSPEED_TEST_GATEWAY=1 uv run --locked pytest tests/test_gateway_live.py
```
