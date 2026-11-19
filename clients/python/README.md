# Lightspeed Python SDK

Start and inspect Lightspeed agent sessions from Python. The SDK is a typed, async
HTTP client generated from the Lightspeed API contract, with helpers for starting
runs, waiting for them to finish, and inspecting what they did. A separate module has
helpers for Temporal workflow code.

It sends the same requests as the
[TypeScript SDK](https://github.com/smartcomputer-ai/lightspeed/tree/main/clients/typescript),
so either one can continue a session or a run the other started.

A runtime token can call every method in every universe, including reading private
sessions, so keep it on your server.

## Connect through Platform

Pass a user or service-user key to the Platform deployment URL. Runtime
operations use the principal's current permissions intersected with key
restrictions. Omit `actor`; Platform derives attribution. Direct runtime tokens
remain full-authority infrastructure credentials.

```python
from uuid import uuid4
from lightspeed_ai import AsyncLightspeedClient

async with AsyncLightspeedClient(
    "https://lightspeed.example", token=platform_key
) as client:
    organizations = await client.platform.list_organizations()
    created = await client.platform.create_universe({
        "organizationId": organizations["organizations"][0]["id"],
        "universeId": str(uuid4()),  # Keep this ID if retrying creation.
        "name": "Experiment",
        "handle": "experiment",
    })
```

Creation requires Creator or Admin access and a credential that permits it. Platform
administrators supply an explicit `initialOwner` when provisioning. Native helpers
take one parameter dictionary using API field names. The same operations are
available through typed `client.call`, for example
`await client.call("platform/organizations/list")`. Native parameter and result types
live in `lightspeed_ai.platform_types`.

For local scripts, reuse the named or selected connection saved by `lightspeed login`
or `lightspeed connect add`:

```python
async with await AsyncLightspeedClient.from_connection("work") as client:
    organizations = await client.platform.list_organizations()
    if client.organization_id is not None:
        organization = await client.platform.read_organization({"organizationId": client.organization_id})
```

The factory verifies the server's credential kind and imports the saved endpoint,
credential, organization and universe together. It does not start login or read ambient
endpoint/token overrides. `config_dir`, `organization` and `universe` can be supplied
explicitly. Changing organizations clears an inherited universe; a selected organization never
silently chooses the server's suggested universe. Organization IDs remain explicit in
native request parameters. Saved CLI session credentials are reread for each request
and renewed only in their owned private file; API keys and externally managed files
are never rewritten. Replacing or removing a saved connection invalidates an existing
imported client.

An imported human CLI session can manage personal keys, or service-user keys when
its user is an organization Admin. API keys cannot mint other keys.

## Install

Requires Python 3.12 or newer. [HTTPX](https://www.python-httpx.org/) is the only
runtime dependency.

Install the stable SDK from PyPI:

```bash
python -m pip install lightspeed-ai
```

The client's types follow the Lightspeed release. Pin `lightspeed-ai==<release-version>`
to match your deployment. For a release that predates PyPI publication, install
from its Git tag:

```bash
python -m pip install "lightspeed-ai @ git+https://github.com/smartcomputer-ai/lightspeed@v<release-version>#subdirectory=clients/python"
```

From a checkout, `python -m pip install ./clients/python` installs the same package.

## Start a session and run a task

You need the gateway's `/api/v1` URL, a
[runtime token](https://ls.bot/docs/access-and-security/api-keys-and-service-access/), and a universe with a default model or a profile to start from.

```python
import asyncio
import os

from lightspeed_ai import AsyncLightspeedClient


async def main() -> None:
    async with AsyncLightspeedClient(
        os.environ["LIGHTSPEED_API_URL"],
        token=os.environ["LIGHTSPEED_RUNTIME_TOKEN"],
        universe=os.environ["LIGHTSPEED_UNIVERSE"],
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

`AsyncLightspeedClient(endpoint, ...)` accepts a deployment origin, base path or
`/api/v1` URL, including a trailing slash. It rejects URL credentials, query parameters
and fragments. Both runtime and Platform requests refuse redirects, including when
a custom HTTPX client normally follows them. The constructor accepts these keyword
arguments:

| Argument | Use |
| --- | --- |
| `token` | Sent as the bearer credential. Omit it only for a gateway in single mode. |
| `universe` | The universe's UUID or handle; supply it for universe operations when constructing a client directly. |
| `actor` | An opaque user ID for attribution. Any direct runtime token may name an actor. |
| `headers` | Extra headers, or a function returning them for each request, which may be async. These override the arguments above. |
| `http_client` | An `httpx.AsyncClient` to send with, for custom transports, timeouts, or tests. |

Use the client as an async context manager, or call `aclose()` when you are done. A
client you pass as `http_client` stays yours to close. Without one, the SDK makes its
own with a 60 second timeout.

To act for several users from one client, use `client.with_actor(user_id)`. It shares
the original's connections. Attribution records who asked, but it does not limit what
the token can do, so your application must check each user's access itself.

## Helpers and `call`

You can use named helpers or operation IDs:

- **Named helpers** cover every runtime operation, including deployment administration,
  credentials, bots, environments and binary transfers.
- **`client.platform` helpers** such as `list_organizations()` and `read_organization({"organizationId": id})` cover native Platform operations.
- **`client.call(operation, params)`** reaches runtime and native Platform operations with generated parameter and result types. Empty parameters can be omitted when the operation has no required fields.

`await client.initialize()` returns `ClientConnectionResponse`, a runtime/Platform
union. When `info["runtime"] == "platform"`, `info["platform"]` contains typed
principal, credential, organization and restriction details.

Every operation has a named helper. `call` remains useful when dispatching by an
operation ID. Both preserve the same wire parameters and response DTOs; the helpers
below add typed positional target IDs and keyword arguments:

```python
# A helper: IDs of the target come first, everything else is a keyword argument, and
# you get the method's data directly.
session = (await client.read_session(session_id, run_limit=5))["session"]
await client.close_session(session_id, force=True)

# call: one params dictionary in the API's own field names, returning the same DTO.
result = await client.call("session/read", {"sessionId": session_id, "runLimit": 5})
print(result["session"])

# Deployment administration is also available through named helpers.
universes = await client.list_universes()
```

Runtime helper availability does not change server authorization. For example,
`client.create_universe(...)` is runtime deployment administration; through Platform,
use `client.platform.create_universe({"organizationId": organization_id, "name": "Work"})`
to apply organization ownership, quota and protection policy. Platform continues to deny
infrastructure and service-only operations where required.

Use `watch` for live list changes and `read_events` for durable session events.
Both layers raise the same
[errors](#handle-errors), and both accept `headers` for one request.

An argument you leave out is not sent. Passing `None` sends `null`, which a method can
treat differently from an absent field, so pass it only when you mean it.

<!-- BEGIN GENERATED HELPERS -->
| Runtime area | Named helpers |
| --- | --- |
| Connection | `initialize` |
| Session | `start_session`, `read_session`, `list_sessions`, `rename_session`, `put_session_metadata`, `put_session_config`, `share_session`, `restart_session`, `close_session`, `delete_session`, `list_runs`, `read_run`, `cancel_run`, `decide_approvals`, `append_context`, `compact_context`, `apply_profile`, `list_session_skills`, `activate_environment`, `deactivate_environment`, `put_session_retention`, `remove_context`, `replace_context`, `start_run`, `steer_run`, `read_events` |
| Profiles | `create_profile`, `read_profile`, `list_profiles`, `put_profile`, `delete_profile` |
| Models | `list_models`, `read_model_defaults`, `put_model_defaults` |
| Environments | `create_environment`, `read_environment`, `list_environments`, `close_environment`, `list_environment_templates`, `bind_environment_credential`, `list_environment_credentials`, `unbind_environment_credential`, `update_environment`, `create_external_environment`, `put_registered_environment`, `release_registered_environment`, `put_environment_ingress`, `put_environment_power`, `list_available_environment_providers`, `read_available_environment_provider`, `read_environment_template`, `create_environment_jobs`, `read_environment_jobs`, `cancel_environment_jobs`, `create_environment_registration_key`, `put_environment_registration_key`, `read_environment_registration_key`, `list_environment_registration_keys`, `update_environment_registration_key`, `rotate_environment_registration_key`, `revoke_environment_registration_key` |
| Vfs | `create_workspace`, `read_workspace`, `list_workspaces`, `delete_workspace`, `read_workspace_file`, `commit_snapshot`, `read_snapshot`, `update_workspace` |
| Mcp | `read_mcp_server`, `list_mcp_servers`, `put_mcp_server`, `discover_mcp_server_auth`, `discover_mcp_server_tools`, `delete_mcp_server` |
| Bots | `read_bot`, `list_bots`, `list_bot_events`, `admit_bot_event`, `create_bot`, `put_bot`, `close_bot`, `delete_bot`, `read_bot_state`, `rotate_bot_session`, `put_bot_trigger`, `read_bot_trigger`, `list_bot_triggers`, `read_bot_trigger_secrets`, `delete_bot_trigger`, `replay_bot_event`, `read_bot_event`, `test_bot_filter` |
| Transcriptions | `start_transcription`, `read_transcription`, `cancel_transcription` |
| Blobs | `put_blobs`, `has_blobs`, `read_blob_bytes`, `upload_blob` |
| Credentials | `import_credential_grant`, `lease_credential_grant`, `read_credential_grant`, `list_credential_grants`, `put_credential_grant`, `revoke_credential_grant`, `create_credential_client`, `read_credential_client`, `list_credential_clients`, `delete_credential_client`, `start_credential_flow`, `read_credential_flow`, `create_credential_provider`, `put_credential_provider`, `read_credential_provider`, `list_credential_providers`, `delete_credential_provider`, `list_github_installations`, `grant_github_installation` |
| Deployment | `list_deleted_universes`, `list_deleted_sessions`, `purge_session`, `list_deployment_environment_provider_bindings`, `create_universe`, `list_universes`, `read_universe`, `put_universe_configuration`, `put_universe_expiry`, `put_universe_handle`, `delete_universe`, `put_environment_provider`, `list_environment_providers`, `read_environment_provider`, `delete_environment_provider`, `put_universe_environment_provider_binding`, `delete_universe_environment_provider_binding` |
| Universe | `watch` |

| Native Platform area | `client.platform` helpers |
| --- | --- |
| Session | `read_current_session`, `revoke_current_session` |
| Credentials | `list_personal_api_keys`, `create_personal_api_key`, `replace_personal_api_key`, `revoke_personal_api_key` |
| Organizations | `list_organization_api_keys`, `list_organizations`, `enroll_organization`, `create_organization`, `read_organization`, `update_organization`, `list_organization_members`, `put_organization_member`, `remove_organization_member`, `list_service_users`, `create_service_user`, `update_service_user` |
| Service-Users | `list_service_user_api_keys`, `create_service_user_api_key`, `replace_service_user_api_key`, `put_service_user_api_key`, `revoke_service_user_api_key`, `list_federated_credentials`, `put_federated_credential`, `delete_federated_credential` |
| Settings | `list_platform_settings`, `put_platform_setting` |
| Universes | `adopt_universe`, `create_universe`, `update_universe`, `list_universe_grants`, `put_universe_grant`, `remove_universe_grant`, `read_platform_universe`, `read_universe_access_policy`, `put_universe_access_policy` |
<!-- END GENERATED HELPERS -->

Some helpers preserve higher-level behavior:

- `start_run` accepts plain text, input items, or the complete `RunStartSource`,
  including `notify_on_terminal` in its options. `steer_run` accepts plain text or
  input items. Both generate a submission ID if you give none.
- `read_events` supports forward and backward reads, including `after`, `before`,
  `direction`, `limit` and `wait_ms`.
- `upload_blob` accepts bytes or an async byte stream; supply `content_length` for
  streams. Its return type is `BlobPutResult`. `read_workspace_file` and
  `read_blob_bytes` return bytes. Use `open_download` for a response stream that
  closes when its async context manager exits.
- `watch` yields typed notifications from named SSE events and reconnects by default.
  It never tries to decode the stream as a JSON response.
- `await_run` has no single method behind it. It follows events, checks run state,
  retries reads, and returns the finished run.
- `put_blob` sends a JSON batch upload; `read_blob_text` and `read_blob_bytes` download raw bytes.

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

The package also exports what it knows about the API itself: `OPERATIONS` and
`NOTIFICATIONS` list the names, `OPERATION_INFO` describes every method, and
`CLIENT_HELPERS` maps all named runtime helpers to their operations;
`MANUAL_CLIENT_HELPERS` identifies the transport and ergonomic implementations.

## Handle errors

| Error | Meaning |
| --- | --- |
| `LightspeedApiError` | The server answered with an error. Check `kind`, such as `notFound`, `conflict` or `sessionExecutionStopped`, and `data`. A missing or invalid runtime token is `unauthenticated`, which the gateway answers with HTTP 401 before reading the request. |
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
        workflow_id = f"{args['universe_id']}/{args['execution_id']}"
        if (
            workflow.info().workflow_id != workflow_id
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
            producer_workflow_id=workflow_id,
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

## Develop this package

The types, method tables, and most helpers are generated from the committed contracts in
`crates/api/contract/` and `crates/temporal-workflow/contract/`, so don't edit
`types.py`, `_generated.py`, `workflow_types.py`, or `_workflow_manifest.py` under
`src/lightspeed_ai/` by hand. Helpers come from the TypeScript SDK's
inventory, [`clients/typescript/scripts/helpers.mjs`](../typescript/scripts/helpers.mjs),
which the generator reads with Node.js. It requires one named helper for every
runtime operation, rejects name collisions and missing implementations, and checks
that handwritten helpers expose the complete wire parameters and transport. The
helper table above is generated from that same inventory. Helpers that need behavior
of their own live in `src/lightspeed_ai/client.py`; native Platform names come from
its shared contract.

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
Alternatively, set `LIGHTSPEED_TEST_CONNECTION_CONFIG` and
`LIGHTSPEED_TEST_MODEL_CONNECTION` to use a saved CLI connection from both SDKs.
Its selected universe needs an agent model default and provider credentials;
this test makes a paid model call.

For a real Platform → runtime test using both SDKs and the Rust CLI, start the
local full stack and build the CLI and TypeScript SDK. From the repository root,
create dedicated fixtures with the seeded development users:

```bash
source scripts/dev/env.sh
LIGHTSPEED_PLATFORM_LIVE_URL=http://localhost:5173 \
LIGHTSPEED_LIVE_CONNECTIONS_DIR=/tmp/lightspeed-live-sdk-connections \
LIGHTSPEED_PLATFORM_LIVE_RENEWAL_DATABASE_URL="$LIGHTSPEED_PLATFORM_DATABASE_URL" \
  npx tsx platform/backend/scripts/check-platform-runtime.ts
cd clients/python
LIGHTSPEED_TEST_CONNECTION_CONFIG=/tmp/lightspeed-live-sdk-connections \
  uv run pytest tests/live_platform_connections.py -v
```

Use the configured browser origin if it differs; the default development UI
proxies API requests to Platform and uses this origin for authentication. The fixture script accepts
`LIGHTSPEED_PLATFORM_LIVE_PASSWORD` for a nondefault development password and
`LIGHTSPEED_LIVE_CLI` for a different CLI binary. It creates an organization and
universes in the development database, then saves dedicated `sdk-session` and
`sdk-key` connections. The renewal-database option ages only the dedicated CLI
session so the first SDK request must renew it. The explicit suite checks
session renewal, JSON, binary and SSE transports,
credential restrictions, cross-universe isolation, and immediate revocation in
both SDKs. **It revokes both fixture credentials**, so create fresh fixtures for
each run; never point it at credentials used by other work. Missing prerequisites
fail rather than skipping. It makes no model calls.

## License

Apache-2.0

HTTP paths, query encoding, and response types are generated from `http.json`. Universe requests require the `universe` client option. `initialize()` returns `defaultUniverse` when the runtime has a single configured universe. `upload_blob` accepts bytes or an async byte iterator with `content_length`; `open_download` is an async context manager yielding a streaming HTTP response. Errors expose HTTP `status`, structured `kind`, and the full problem `payload`.
