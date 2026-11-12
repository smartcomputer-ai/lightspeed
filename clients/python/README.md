# Lightspeed Python SDK

Start and inspect Lightspeed agent sessions from Python. The SDK is a typed, async
JSON-RPC client generated from the Lightspeed API contract, with helpers for starting
runs, waiting for them to finish, and inspecting what they did. A separate module has
helpers for Temporal workflow code.

It sends the same requests as the
[TypeScript SDK](https://github.com/smartcomputer-ai/lightspeed/tree/main/clients/typescript),
so either one can continue a session or a run the other started.

An API key with the `session` group can read and control every session in its
universe, including private ones, so keep it on your server.

## Install

Requires Python 3.12 or newer. [HTTPX](https://www.python-httpx.org/) is the only
runtime dependency.

Install from the repository, at the tag of your Lightspeed release, because the
client's types follow that release's API:

```bash
python -m pip install "lightspeed-ai @ git+https://github.com/smartcomputer-ai/lightspeed@v<release-version>#subdirectory=clients/python"
```

From a checkout, `python -m pip install ./clients/python` installs the same package.

## Start a session and run a task

You need the gateway's `/rpc` URL, an
[API key](https://ls.bot/docs/access-and-security/api-keys-and-service-access/) with the
`session` group, and a universe with a default model or a profile to start from.

```python
import asyncio
import os

from lightspeed_ai import AsyncLightspeedClient


async def main() -> None:
    async with AsyncLightspeedClient(
        os.environ["LIGHTSPEED_API_URL"],
        api_key=os.environ["LIGHTSPEED_API_KEY"],
    ) as client:
        started = await client.start_session(
            display_name="Repository summary",
            metadata={"application": "my-app"},
        )
        session_id = started["session"]["id"]

        submitted = await client.start_run(session_id, "Summarize this repository.")
        run_id = submitted["run"]["id"]

        async with asyncio.timeout(5 * 60):
            result = await client.await_run(
                session_id,
                run_id,
                on_event=lambda event: print(event["kind"]["type"]),
            )

        run = result["run"]
        if run["status"] == "completed":
            print(run.get("outputText"))
        else:
            print(run["status"], run.get("failure"))


asyncio.run(main())
```

Giving up on the wait does not cancel the run. It keeps going, and you can read or
wait for it again later. To cancel it, call `cancel_run`.

Results are plain dictionaries, typed as `TypedDict`s. Their keys are the API's own
field names, such as `outputText`, and an optional field can be absent, so read those
with `.get`.

## Retry and resume

For work that must survive a lost response or a restart, supply your own `session_id`
and `submission_id`. Repeating a call with the same IDs and input finds the same
session and run instead of starting new ones:

```python
await client.start_session(session_id="invoice-1234", display_name="Invoice 1234")
submitted = await client.start_run(
    "invoice-1234",
    "Check this invoice against the purchase order.",
    submission_id="invoice-1234-check",
)
```

`await_run` returns the cursor it reached along with the run. Save it, and pass it as
`after` to pick up the session's events from there, in this process or another:

```python
result = await client.await_run("invoice-1234", submitted["run"]["id"])
saved_cursor = result["cursor"]

# Later, for the session's next run.
result = await client.await_run("invoice-1234", next_run_id, after=saved_cursor)
```

The concepts are the same in every client, and
[Build with the API and TypeScript client](https://ls.bot/docs/integrating-and-extending/api-and-typescript/)
covers them in depth.

## Connect

`AsyncLightspeedClient(endpoint, ...)` takes the gateway's `/rpc` URL and these keyword
arguments:

| Argument | Use |
| --- | --- |
| `api_key` | Sent as the bearer credential. Omit it only for a gateway in single mode. |
| `universe` | The universe's UUID or handle, for a deployment key only. A universe key must leave it out. |
| `actor` | An opaque user ID to attribute requests to. The key must be created with `assertActor`. |
| `headers` | Extra headers, or a function returning them for each request, which may be async. These override the arguments above. |
| `http_client` | An `httpx.AsyncClient` to send with, for custom transports, timeouts, or tests. |
| `request_id` | A function supplying each request's JSON-RPC ID, in place of a counter. |

Use the client as an async context manager, or call `aclose()` when you are done. A
client you pass as `http_client` stays yours to close. Without one, the SDK makes its
own with a 60 second timeout.

To act for several users from one client, use `client.with_actor(user_id)`. It shares
the original's connections. Attribution records who asked, but it does not limit what
the key can do, so your application must check each user's access itself.

## Helpers and `call`

The client has two layers, and you can mix them freely:

- **Named helpers** such as `client.read_session(session_id)` cover what most
  applications need.
- **`client.call(method, params)`** reaches every method in the API, typed, by its name.

A helper and its `call` do the same thing. Prefer the helper when there is one, and use
`call` for everything else, mainly administration. The two differ in shape:

```python
# A helper: IDs of the target come first, everything else is a keyword argument, and
# you get the method's data directly.
session = (await client.read_session(session_id, run_limit=5))["session"]
await client.close_session(session_id, force=True)

# call: one params dictionary in the API's own field names, and you get the whole
# outcome.
outcome = await client.call("session/read", {"sessionId": session_id, "runLimit": 5})
print(outcome["result"]["session"], outcome.get("notifications"))

# Administration has no helper.
keys = await client.call("deployment/api-keys/list", {})
```

Use `call` if you need the `notifications` returned with a method; helpers leave them
out. The durable session event stream is the same either way. Both layers raise the same
[errors](#handle-errors), and both accept `headers` for one request.

An argument you leave out is not sent. Passing `None` sends `null`, which a method can
treat differently from an absent field, so pass it only when you mean it.

| Area | Helpers |
| --- | --- |
| Connection | `initialize` |
| Sessions | `start_session`, `read_session`, `list_sessions`, `rename_session`, `put_session_metadata`, `put_session_config`, `share_session`, `restart_session`, `close_session`, `delete_session` |
| Runs | `start_run`, `await_run`, `read_run`, `list_runs`, `cancel_run`, `steer_run`, `decide_approvals`, `read_events` |
| Session setup | `append_context`, `compact_context`, `apply_profile`, `list_session_skills`, `activate_environment`, `deactivate_environment` |
| Content | `put_blob`, `read_blob_text`, `read_blob_bytes` |
| Profiles | `create_profile`, `read_profile`, `list_profiles`, `put_profile`, `delete_profile` |
| Models | `list_models`, `read_model_defaults` |
| Environments | `create_environment`, `read_environment`, `list_environments`, `close_environment`, `list_environment_templates` |
| Workspaces | `create_workspace`, `read_workspace`, `list_workspaces`, `delete_workspace`, `read_workspace_file` |
| MCP servers | `read_mcp_server`, `list_mcp_servers` |
| Bots | `read_bot`, `list_bots`, `list_bot_events`, `admit_bot_event` |

Everything else stays on `call`: auth grants and providers, deployment and universe
management, channels, registration keys, transcription, and the remaining bot, MCP, and
environment operations. A few helpers do more than wrap one method:

- `start_run` and `steer_run` accept plain text and generate a submission ID if you
  give none.
- `await_run` has no single method behind it. It follows events, checks run state,
  retries reads, and returns the finished run.
- `put_blob`, `read_blob_text`, and `read_blob_bytes` handle the base64 encoding.

Each helper's docstring carries the method's documentation and describes its arguments.
The
[API reference](https://github.com/smartcomputer-ai/lightspeed/blob/main/crates/api/contract/api-reference.md)
covers every method in full.

## Wait for a run

`await_run` returns `{"run": ..., "cursor": ...}` once the run has finished, where `run`
is the same view `read_run` returns.

- `on_event` receives the run's own events. `on_page` receives every event page for the
  session, and `heartbeat` receives the cursor after each page. Each may be a plain
  function or an async one.
- Pass `after` with a saved cursor to resume. Without it, the wait reads the session
  from its beginning.
- A run that has already finished returns at once, whatever the cursor.
- Reads are repeated through network failures and stalled connections for about 30
  seconds before the wait raises.
- To stop waiting, wrap the call in `asyncio.timeout` or cancel its task. Nothing is
  sent to the run.

## Inspect a run

`read_run` returns a run's messages and tool calls as separate lists. `run_timeline`
merges them into one list in the order they happened, with each tool call joined to its
result:

```python
from lightspeed_ai import run_timeline

run = (await client.read_run(session_id, run_id))["run"]

for item in run_timeline(run):
    if item["type"] == "message":
        print(item["role"], item["entry"].get("text"))
    elif item["type"] == "tool" and item["call"] is not None:
        call = item["call"]
        print(item["name"], call["status"], call.get("durationMs"))
        # Output here is a bounded preview. The whole of it is a blob.
        output_ref = call.get("outputRef")
        if call.get("outputPreviewTruncated") and output_ref:
            print(await client.read_blob_text(output_ref))
```

`read_blob_text` raises `UnicodeDecodeError` if the blob is not UTF-8. `read_blob_bytes`
returns the raw bytes.

To keep a view current, follow the run with `await_run` and read it again when
`on_event` fires, rather than building the view from events yourself.

## Types

`lightspeed_ai.types` holds every type in the API, named as the API names them and
documented from the contract. Each variant of a tagged union is named after its tag,
and checking the tag narrows the type:

```python
from lightspeed_ai.types import SessionEventView


def failure_message(event: SessionEventView) -> str | None:
    kind = event["kind"]
    if kind["type"] == "runFailed":
        # kind is a SessionEventKindViewRunFailed here.
        return kind["message"]
    return None
```

The package also exports what it knows about the API itself: `METHODS` and
`NOTIFICATIONS` list the names, `METHOD_INFO` describes every method, and
`CLIENT_HELPERS` maps each generated helper to its method.

## Handle errors

| Error | Meaning |
| --- | --- |
| `LightspeedRpcError` | The server answered with an error. Check `kind`, such as `notFound`, `conflict` or `sessionExecutionStopped`, and `data`. |
| `LightspeedTransportError` | The request failed or the response was unusable. `status` and `body` are set when there was a response, `__cause__` keeps the underlying failure, and `transient` says the same request may succeed later. |
| `TimeoutError`, `asyncio.CancelledError` | Your own `asyncio.timeout` passed, or you cancelled the task. |

Only `await_run` retries, and only its own reads. After a transport failure on a
mutation, the request may still have landed, so repeat it with the same IDs rather than
new ones.

## Write workflow code

Temporal workflows that receive tool calls from an integrated session import
`lightspeed_ai.workflow`. It has the contract's signal, query and activity names, and
helpers for parsing envelopes, deriving IDs, fingerprinting recipes, and building
replies. `lightspeed_ai.workflow_types` has the generated types, which work as the type
hints Temporal decodes workflow arguments and signals from, such as
`WorkflowToolStartArgs` and `EmissionEnvelope`. The module depends on no Temporal
package and is deterministic. Its functions raise `ValueError` for a value the contract
does not allow.

Temporal's Python SDK runs workflow code in a sandbox, so import both modules as
pass-throughs. This is the started workflow from
[Workflow tools](https://ls.bot/docs/integrating-and-extending/workflow-tools/#implement-the-started-workflow),
which describes the integration, in Python. Its `review_release` activity is application
code that reads the argument blob, performs the review, uploads a result, and returns
its blob reference:

```python
from datetime import timedelta

from temporalio import workflow
from temporalio.exceptions import ApplicationError

with workflow.unsafe.imports_passed_through():
    from lightspeed_ai.workflow import (
        DELIVER_EMISSION_SIGNAL,
        REPLY_COMPLETION_KEY,
        WORKFLOW_TOOL_RECOVERY_QUERY,
        reply_promise_id,
        source_resolution_envelope,
    )
    from lightspeed_ai.workflow_types import (
        PromiseResolution,
        WorkflowToolRecoveryResult,
        WorkflowToolStartArgs,
    )


@workflow.defn
class ReleaseReviewWorkflow:
    def __init__(self) -> None:
        self.resolutions: dict[str, PromiseResolution] = {}

    @workflow.query(name=WORKFLOW_TOOL_RECOVERY_QUERY)
    def recovery(self) -> WorkflowToolRecoveryResult:
        return {"resolutions": self.resolutions}

    @workflow.run
    async def run(self, args: WorkflowToolStartArgs) -> WorkflowToolRecoveryResult:
        invocation = args["invocation"]
        if (
            workflow.info().workflow_id != args["execution_id"]
            or args["universe_id"] != invocation["session_universe_id"]
            or invocation["workflow_tool_id"] != "request-review"
            or invocation["semantic_type"] != "release.review.v1"
            or invocation["schema_revision"] != 1
        ):
            raise ApplicationError("Unexpected review invocation", non_retryable=True)
        promise_id = reply_promise_id(invocation)

        result_ref: str = await workflow.execute_activity(
            "review_release",
            {
                "universe_id": args["universe_id"],
                "arguments_ref": invocation["arguments_ref"],
                "invocation_id": invocation["invocation_id"],
            },
            start_to_close_timeout=timedelta(minutes=2),
        )
        resolution: PromiseResolution = {"kind": "resolved", "payload_ref": result_ref}
        self.resolutions[REPLY_COMPLETION_KEY] = resolution

        reply = source_resolution_envelope(
            universe_id=args["universe_id"],
            producer_workflow_id=args["execution_id"],
            holder_workflow_id=args["holder_workflow_id"],
            promise_id=promise_id,
            resolution=resolution,
        )
        holder = workflow.get_external_workflow_handle(args["holder_workflow_id"])
        await holder.signal(DELIVER_EMISSION_SIGNAL, reply)
        return {"resolutions": self.resolutions}
```

Store each final resolution under its completion key before sending it, because the
recovery query answers from those keys. A bound receiver takes `EmissionEnvelope` in its
`DELIVER_EMISSION_SIGNAL` handler and checks it with `parse_emission_envelope`.

## Not in this package

The TypeScript SDK also ships a request handler for serving a browser UI without
exposing the key. It has no Python equivalent: a Python application that renders its UI
on the server calls this client there.

## Develop this package

The types, method tables, and most helpers are generated from the committed contracts in
`crates/api/contract/` and `crates/temporal-workflow/contract/`, so don't edit
`types.py`, `_generated.py`, `workflow_types.py`, or `_workflow_manifest.py` under
`src/lightspeed_ai/` by hand. Helpers come from the TypeScript SDK's
inventory, [`clients/typescript/scripts/helpers.mjs`](../typescript/scripts/helpers.mjs),
which the generator reads with Node.js; add a new one there and to the table above. A
helper that needs behavior of its own goes in `src/lightspeed_ai/client.py`.

With [uv](https://docs.astral.sh/uv/) and Node.js installed, from `clients/python`:

```bash
uv sync --locked
uv run python scripts/generate.py           # after a contract changes
uv run python scripts/generate.py --check   # fails if the committed output is stale
uv run mypy
uv run pytest
```

`tests/test_gateway_live.py` starts a run with the TypeScript SDK and finishes it from
Python. It is skipped unless `LIGHTSPEED_TEST_GATEWAY=1` is set, and it needs a built
TypeScript SDK and `LIGHTSPEED_API_URL` pointing at a running gateway.

## License

Apache-2.0
