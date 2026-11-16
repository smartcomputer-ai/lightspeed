# Build with the API and TypeScript client

An application submits work to a Lightspeed session and observes it until the
run finishes. The session owns the durable conversation and configuration;
the application owns its business operation, the identifiers used to retry
it, and what to do with the result. Stable identifiers let the application
recover from a lost HTTP response without creating duplicate work.

The TypeScript client supplies typed HTTP calls and helpers for starting
runs and waiting for them to finish. Its runtime types come from the Rust API contract; Platform-native types come
from the shared Platform contract.
Build against this client and the public contract so runtime implementation
changes do not become dependencies of your application. A
[Python client](../../../clients/python/README.md) covers the same API; what this
page says about identifiers, events, and lifecycle applies to it too.

## Platform access

Use a personal or application API key against the Platform `/api/v1` base URL
for scoped access. Runtime resource routes use the same generated catalog as
direct runtime access. Platform resolves the principal, checks current account
and universe roles, and intersects those roles with credential restrictions.
It forwards admitted requests with its own infrastructure token and trusted
actor attribution. Clients must not supply actor headers to impersonate users.

Platform-native account, application, grant, and credential routes live under
`/api/v1/platform`. Their schemas and route descriptors are authored once in
`platform/shared/src/platform-api.ts` and generated into both SDKs. Use
`client.call(operation, params)` in either SDK for both runtime operation IDs,
such as `profiles/list`, and native Platform IDs, such as
`platform/accounts/read`. Generated `client.platform` helpers expose the same
native operations with typed parameter objects: TypeScript
`client.platform.readAccount({ accountId })` and Python
`client.platform.read_account({"accountId": account_id})`. Native operations
require a Platform connection; direct runtimes do not serve them.
Personal and application keys are managed from the web UI or
`lightspeed platform api-key`. Use `lightspeed connect login --url DEPLOYMENT_URL`
for browser-approved CLI access. See [People and roles](../access-and-security/people-and-roles.md).

## Reuse a saved CLI connection

For a local script, first select the connection and resources with the CLI:

```bash
lightspeed connect login --url https://lightspeed.example --name work
lightspeed platform account use ACCOUNT_ID
lightspeed universe use my-universe
```

The SDK factories load the same named connection, pinned endpoint, credential
file, and selected account/universe. Omit the name to use the CLI's selected
connection. They do not open a browser or start a login flow.

In Node.js, import the factory from the Node-only entry point:

```ts
import { createClientFromConnection } from "@lightspeed-ai/sdk/node";

const client = await createClientFromConnection({ name: "work" });
if (!client.accountId) throw new Error("Select an account with lightspeed platform account use");
const { account } = await client.platform.readAccount({ accountId: client.accountId });
const profiles = await client.call("profiles/list", {});
console.log(account.name, profiles);
```

In Python, use the asynchronous factory:

```python
import asyncio
from lightspeed_ai import AsyncLightspeedClient

async def main():
    async with await AsyncLightspeedClient.from_connection("work") as client:
        if client.account_id is None:
            raise RuntimeError("Select an account with lightspeed platform account use")
        account = await client.platform.read_account({"accountId": client.account_id})
        profiles = await client.call("profiles/list", {})
        print(account, profiles)

asyncio.run(main())
```

Both factories reread the credential for each request and handle renewal of
CLI-owned session credentials. They do not overwrite externally managed secret
files. A revoked or expired login requires an explicit `lightspeed connect login`;
there is no automatic login or separate SDK refresh-token protocol. API keys
retain their own expiry and restrictions. See
[CLI connections](../using-lightspeed/cli.md#connect-to-platform).

The selected account is exposed as `client.accountId` / `client.account_id`.
Pass it explicitly in native helper parameter objects; selection does not
silently fill account fields or grant access. The main TypeScript import stays
browser-compatible and does not read local CLI files.

## Resource HTTP API

The private runtime listener serves the resource API under `/api/v1`.
Its [OpenAPI contract](../../../crates/api/contract/openapi.json) and
[routing catalog](../../../crates/api/contract/http.json) are generated from
the same Rust declarations as its handlers. The SDKs and Platform use this
contract for routing, types, and operation policy metadata.

Authenticate with the same configured bearer token. Select the universe in
the URL, using its UUID or handle, and omit `x-lightspeed-universe`:

```bash
curl --fail-with-body \
  -H "Authorization: Bearer $LIGHTSPEED_API_TOKEN" \
  http://localhost:18080/api/v1/connection

curl --fail-with-body \
  -H "Authorization: Bearer $LIGHTSPEED_API_TOKEN" \
  "http://localhost:18080/api/v1/universes/$LIGHTSPEED_UNIVERSE/sessions?limit=20"
```

Resource responses contain the declared DTO directly. Failures use an HTTP
status and `application/problem+json`, including the stable `kind` field and
structured details such as a retry's `submissionId`. A request timeout does
not prove that a submitted command was rejected; keep its submission identity
when retrying.

Path identifiers are authoritative: do not repeat them in a write body,
including inside nested records. Query arrays use repeated parameters;
structured cursor/filter values use URL-encoded JSON where the operation
declares that encoding. Unknown or duplicate scalar parameters are rejected.

`GET /api/v1/universes/{universe}/events` serves the universe watch with
`Accept: text/event-stream`. It retains reset/refetch semantics and is not a
durable replay log. Blob reads return binary bytes; single binary uploads use
`PUT /api/v1/universes/{universe}/blobs/{blobRef}/content` with
`Content-Type: application/octet-stream` and `Content-Length`. Uploads spool
to a temporary file to verify the digest and length before storage admission.
The batch blob upload remains a separate JSON operation. Range requests are
not supported by these initial HTTP bindings.

Server integrations can import generated `HTTP_OPERATIONS` and
`HTTP_CONTRACT_REVISION` from `@lightspeed-ai/sdk/server`; the package also
ships `schema/http.json` and `schema/openapi.json`. These describe routes and
policy targets without adding a second runtime body validator. They do not
provide the Platform's authentication or resource authorization.

## Prepare an endpoint and a profile

For this example, provide an authenticated runtime URL ending in `/api/v1`, a
configured runtime token, and the target universe's UUID or handle. This is a
direct connection with full authority over every method and universe. Follow
[Runtime tokens and service access](../access-and-security/api-keys-and-service-access.md)
to configure it.

Run this integration on a trusted application server. A runtime token can read
private sessions and change resources in every universe; keep it out of browser
code, agents, and untrusted applications. An application serving people must
authenticate them and enforce its own access policy before forwarding requests.
The Platform already does this for its web app. `client.withActor(userId)` records
who asked; every runtime token may set an actor, and attribution grants no
permissions. See [Serve a browser UI](#serve-a-browser-ui) for the gateway boundary.

Create the `release-editor` profile and `release-notes` workspace from
[Build your first agent](../getting-started/first-agent.md), including
`changes.md` and a working model connection. The example reuses that setup
to prepare the Acorn 1.2 notes from an application.

Use Node.js 24 or newer. Tagged releases publish `@lightspeed-ai/sdk`, and inside this
repository the package is also available as a workspace dependency. We recommend pinning
the version that matches your deployed Lightspeed release, because the client's types
follow that release's API:

```bash
npm install --save-exact "@lightspeed-ai/sdk@<release-version>"
```

Replace `<release-version>` with the actual package version, or run
`npm install @lightspeed-ai/sdk` for the latest release. Supply
`LIGHTSPEED_API_URL`, `LIGHTSPEED_API_TOKEN`, and `LIGHTSPEED_UNIVERSE` through the
application's protected configuration. The `universe` option names the UUID or
handle in resource paths; connection and deployment operations do not use
that selection. The example also reads `ACORN_RELEASE_JOB_ID`, an
application-defined operation ID such as `acorn-1.2-draft-001`. Keep it unchanged
when retrying that operation.

## Submit and observe one task

Save this as `release-notes.mts`. The session and submission IDs
allow the same script to find the same work after a network failure:

```ts
import { LightspeedClient } from "@lightspeed-ai/sdk";

function required(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`Missing ${name}`);
  return value;
}

const jobId = required("ACORN_RELEASE_JOB_ID");
const client = new LightspeedClient({
  endpoint: required("LIGHTSPEED_API_URL"),
  token: required("LIGHTSPEED_API_TOKEN"),
  universe: required("LIGHTSPEED_UNIVERSE"),
});

const { session } = await client.startSession({
  sessionId: `release-${jobId}`,
  displayName: "Acorn 1.2 release notes",
  access: { visibility: "universe" },
  profile: { kind: "named", profileId: "release-editor" },
  metadata: { application: "release-pipeline", jobId },
});
const sessionId = session.id;

const started = await client.startRun(
  sessionId,
  "Read changes.md in the release-notes workspace, write the Acorn 1.2 " +
    "release-notes.md draft, and return a short summary of what you wrote.",
  { submissionId: `draft-${jobId}` },
);
const runId = started.run.id;
console.log({ sessionId, runId, status: started.run.status });

const { run } = await client.awaitRun(sessionId, runId, {
  signal: AbortSignal.timeout(5 * 60_000),
  onEvent(event) {
    console.log(event.cursor.seq, event.kind.type);
  },
});

if (run.status !== "completed") {
  throw new Error(`Run ${runId} ended with status ${run.status}: ${run.failure?.message}`);
}
console.log(run.outputText ?? "Completed without a text output");
```

Run it with the configured environment:

```bash
node release-notes.mts
```

The start response means the run was admitted. It can be queued behind
existing work or already running; it does not mean generation finished.
`awaitRun` follows session events until the requested run completes, fails, or is
canceled, then returns that run's projection, including its complete visible
terminal text in `outputText` when present, and a `failure` with `kind` and
`message` when it failed.

The example creates a session shared with the universe so teammates can inspect
it in the Platform. Omitting `access` creates private work; because this client
names no user, only admins can see that session through the Platform.
Open the session and inspect `release-notes.md` to verify the workspace change.
The run's final message and a file it wrote are different outputs. Reuse the
session for a follow-up conversation, or create another session ID for
independent work.

## Use helpers or `call`

Every public runtime and native Platform operation has a typed named helper in
both SDKs. Runtime helpers live directly on `client`; `client.platform` supplies
native account, application, credential and access helpers such as
`client.platform.readAccount({ accountId })`. Python uses matching snake_case
names. The generators check complete coverage against both operation catalogs.

Prefer named helpers for application code. `client.call(operation, params)`
remains a typed alternative for code that selects operations by ID. Runtime
helpers take target IDs first and the remaining settings in an options object;
generated `client.platform` helpers take one typed parameter object. Both return
the operation's data. Python runtime helpers use keyword arguments for settings.

A few helpers add behavior: `startRun` and `steerRun` accept plain text and
generate a submission ID, and `startRun` also accepts every structured run source
and terminal notification setting. `awaitRun` combines several operations.
`watch` streams events, blob downloads return bytes, and `uploadBlob` accepts a
binary body. `putBlobs` exposes batch blob storage, while `putBlob` stores one
value and returns its reference. These helpers keep the same server permissions
and endpoint availability as their underlying operations. Platform admission
still controls which runtime operations are available and applies account and
universe policy; account-qualified universe creation uses
`client.platform.createUniverse`. The
[client README](../../../clients/typescript/README.md#helpers-and-call) lists them all.

## Transcribe audio before submitting session input

Run start, context append, and steering reject raw audio media. Transcribe
audio separately, then submit ordinary `text` or `textRef` input:

1. Upload the audio to the universe's content store with `blobs/put`.
2. Call `transcriptions/start` with a stable `idempotencyKey` and an `audio`
   object containing `blobRef`, `mime`, and `name`. Supply a complete `model`
   route, or omit it to resolve the universe's **Speech-to-text** default.
   The API kind must be `openai:audio-transcriptions`.
3. Save the returned `transcriptionId` and poll `transcriptions/read` until
   `status` is terminal. `succeeded` supplies plain UTF-8 `text` and
   `transcriptRef`. Handle `failed`, `cancelled`, and `expired` explicitly;
   `failure` provides details when present.
4. Submit the prepared text through the ordinary session API. For unchanged
   transcripts, an optional `provenanceRef` can point to the source audio blob
   in the same universe. Admission checks that the source exists and retains
   it with the session content.

Transcription creates no session or run. A direct runtime token can call all
of these methods in the selected universe.
Use `transcriptions/cancel` to request cancellation; abandoning an HTTP wait
does not cancel the job.

Matching retries with the same requester, idempotency key, audio, and options
rejoin the original job even if the universe default has changed. Reusing the
key with changed input conflicts. This identity lasts for the Temporal
namespace's workflow-history retention period. The resolved model is fixed
at admission. The Platform allows people to read and cancel only their own
drafts. Direct runtime access applies no actor-based ownership check; an
application gateway must enforce that rule itself.

Unsubmitted audio and transcripts use ordinary CAS collection grace and can
expire. Persist or admit the result when it is needed beyond that period.
When users edit a transcript, send their reviewed text as an ordinary message;
do not present the edited words as the unchanged transcription.

## Keep retry identity with the business operation

Persist the session ID, submission ID, request input/configuration, and returned
run ID with your application's job record. The example derives stable IDs
from that record's job ID. If the response is lost, repeat the same request
with the same identifiers and contents.

`session/start` with an existing ID returns that session. It does not reapply
the creation profile or replace its configuration. Reusing a session ID for a
different business operation can therefore reconnect to old state. Creation
retries can still validate supplied profile/configuration references, so an
invalid or deleted referenced profile can prevent a retry. A retry also
[restarts a stopped workflow execution](../deployment/upgrades-and-recovery.md#restart-a-stopped-session)
from the log, except for a session created with an `integration`: that retry reports
`sessionExecutionStopped`. Once IDs are saved, read the known session/run directly during recovery
instead of unnecessarily repeating creation. If a start fails during setup, a
server-allocated ID is closed and a client-supplied one reports the same error on retry.

`session/runs/start` deduplicates by submission ID within the session. The
same ID and source/configuration/terminal notification return the original
run. Reusing the ID with changed inputs is rejected. Allocate a new submission
ID when asking for another run. The session remembers the last 256 finished runs for
this, so a retry much later than that starts a new run. `session/runs/steer` takes a
`submissionId` too, and remembers the active run's last 64 steering batches.

The `startRun` helper generates an ID if you omit one, but calling the helper
again generates another ID. That default does not provide retry safety across
application restarts. HTTP requests themselves do not identify durable work;
they do not replace submission IDs. Apart from the reads inside `awaitRun`, the
client does not retry failed HTTP calls.

A timeout or aborted HTTP wait also does not cancel the durable run. The
five-minute timeout above bounds this client's waiting time. On recovery,
read the known run first: if it is terminal, use its result; otherwise resume
observation or request cancellation.

## Follow events and reconnect

`readEvents(sessionId, { after, limit, waitMs })` reads chronological event
pages. `after` is an exclusive sequence cursor. With `waitMs`, an empty tail
read waits for events or the long-poll timeout and then returns an ordinary
page. An empty page is not a run completion.

Process a page's events before saving its continuation cursor. Persist the
cursor with the effects your application derives from those events, or make
those effects idempotent, so a restart can safely repeat a partially processed
page. Preserve event joins such as `runId` when several runs share a session.

`awaitRun` provides `onEvent`, `onPage`, and `heartbeat` callbacks and accepts
an `after` cursor. `onEvent` receives the run's own events, while `onPage` receives
every event in the session. The example omits `after`, so it reads the retained
stream from the beginning; pass the `cursor` from an earlier wait when a session has
a long history. When a read finds no new events, the helper checks the run's state,
so a run that finished behind its cursor returns at once, and a session whose
workflow has stopped rejects with `sessionExecutionStopped`. Reads are repeated
through network failures and stalled connections for about 30 seconds before the
wait rejects.

`complete` means the requested direction has no more events at the instant
of the read; it does not mean the session is closed.

For a transcript UI, `session/events/read` also supports backward pagination.
Use `nextCursor` as `before` to load older history, and use the initial page's
`headCursor` as `after` for the forward live stream. Keep those two cursor
directions separate. Pages can split a run or tool batch, so retain enough
projection state to join their pieces.

## Build transcript authors and times

The TypeScript SDK exports `transcriptMetadata` and `transcriptAuthor`. They are
pure helpers over the existing public event types: no requests, stored timestamps,
React state or rendering decisions. The Platform web already imports this SDK;
its transcript fold can use these helpers when adding author and time display.

```ts
import { transcriptMetadata } from "@lightspeed-ai/sdk";

// loadedEvents is the union of history pages and live SessionEventView events.
const metadataByEntryId = transcriptMetadata(sessionId, loadedEvents);

// Reuse the projection for every displayed message.
const metadata = metadataByEntryId.get(entry.id);
```

Pass the union of the loaded history pages and live events, then look up each
message by its context entry ID. The helper ignores other sessions and handles
unsorted, overlapping pages. Recompute when older history or live events arrive;
it does not retain previous calls. It performs one indexing pass and one pass over
message entries. The returned map supplies metadata, not transcript order.

| Result field | Meaning |
| --- | --- |
| `sentAtMs` | Server acceptance time for run or steering input, otherwise the earliest loaded context event carrying the message. |
| `timeSource` | `runAccepted`, `runSteeringAccepted`, or `contextEvent`. The last is provisional for input whose acceptance event is not loaded. |
| `author` | An actor `{ kind: "actor", id, source }`, a bot delivery `{ kind: "event" }`, or `null`. Assistant messages have no user author. |
| `requestedBy` | Full attribution from the matching acceptance event, or `null`; preserves actor, token, local and internal cases. |

If input is accepted at 10:00 and enters context at 10:02 after waiting in the
queue, its `sentAtMs` is 10:00. Steering joins by both run ID and steering ID;
assistant output and context edits do not borrow a run's acceptance time. When
an older page supplies the missing acceptance, recomputing corrects the time and
author. Repeated context entries use their earliest loaded sequence, even if a
later event's wall clock is lower. A queued message not yet applied to context has
no entry ID and is not returned; its acceptance event still contains its input.

For user messages, an authenticated `requestedBy` actor takes precedence over a
`user:<id>` origin. The author's `source` distinguishes those cases. Other
attribution kinds are not converted into people, and `origin=event` stays a bot
delivery. `transcriptAuthor(origin, requestedBy?)` exposes the author rule alone,
which also works for a user-role list preview. Origin is display metadata, never
proof of identity or permission.

Platform sets `user:<id>` from the signed-in user on message and steering input.
Its event endpoint preserves the runtime fields, and
`GET /api/ui/universes/:id/members` supplies IDs, current names and roles; only
admins receive emails. Resolve actor IDs through that data in the application.
An unmatched ID may be a former member or an external application's actor, so do
not assume it identifies a former Platform member. Names, labels, date formatting
and which authors to show remain presentation decisions.

For optimistic sends, keep a stable local render key and attach backend IDs as
acknowledgements arrive. Run acceptance exposes `submissionId`; steering acceptance
exposes it in `joins.submissionId`, alongside `kind.steeringId`. Context entries carry
the matching run/steering source. Use those IDs for reconciliation, not message text.
A POST receipt can precede the corresponding transcript events, including when it
already reports completion; retain the local message until the event reader catches
up. These render identities belong to the current view, not durable session state.

## Keep session and resource lists current

The Platform browser calls its cookie-authenticated `/api/v1` routes. Platform
checks membership and session visibility, then calls the Rust runtime through the
SDK with its runtime token and the member's actor ID. The browser uses SDK types
and pure helpers without holding that token.

| View | Available data and refresh path |
| --- | --- |
| Session list | `SessionSummaryView` from `session/list` and `session/changed`: activity, `activityPhase`, `activitySinceMs`, last run, execution failure, message preview/role/origin/time, attached resource IDs and revision. |
| Bot roster | `BotListItem` from `bots/list` and `bots/changed`: status, activity and counts. Detailed controller state still uses `bots/state/read`. |
| Environments | `EnvironmentView` from `environments/list` and `environments/changed`. |
| Workspaces | `VfsWorkspaceView` from `vfs/workspaces/list` and `workspaces/changed`: name, revision, head snapshot and totals. Refresh an open tree when its head changes; load files by immutable blob reference. |
| Transcript | Paginated `session/events/read` plus its forward long-poll tail. List notifications do not replace transcript events. |

Session summaries are materialized with store writes; callers do not reconstruct
activity or preview text by replaying history. `lastMessageAtMs` is when that
preview entered context, so it can differ from input acceptance time. Use
`lastMessageRole` before interpreting `lastMessageOrigin` as a user author. An
empty or unavailable message can leave the previous preview intact. Display
elapsed activity time locally from `activitySinceMs`.

`universe/watch` is `GET /api/v1/universes/{universe}/events` with `Accept: text/event-stream`;
the TypeScript and Python SDKs expose watch iterators. Platform offers
the same resource route. The current web provider shares one connection
across pages in the active universe of each tab. It receives all authorized
session rows, including ones excluded by the current list filters. Ordinary list
reads still perform filtering and pagination on the server; clients apply the
same view filters when updating cached rows.

Replace changed rows by ID, remove rows on `removed`, and refetch visible queries
on `watch/reset`, including the reset that starts each connection. Restricted
streams receive reset rather than a deleted session ID whose access is no longer
known. Session and workspace revisions reject older updates. If a change overlaps
an HTTP fetch, preserve it until that response has been applied; the current web
watch cache already handles this. Keep slow fallback reads because signals are
lossy and have no replay cursor. Platform rotates connections after 60 seconds to
recheck membership and feature access; direct runtime streams last at most 15
minutes. These streams carry resource rows, not model tokens.

## Read content through the public projection

`session/read` returns stored session state and a bounded recent run-summary page,
and needs no worker. `lifecycleStatus` is `new`, `open` or `closed`, `activity` says what
the session is doing, and `executionFailure` appears when an open session's workflow
has stopped. Follow its run cursor through `session/runs/list` for older summaries;
those come from the log and have no usage figure. `session/runs/read` provides one
run's detail. A run past the detail limit keeps its opening and latest whole turns and
sets `earlierEventsOmitted`, and `session/events/read` serves the rest.

Run start, cancel, steer and decide all return a run summary, including `failure` when
there is one, so a caller rarely needs a second read.

Use `outputText` for visible terminal text. A `ContentRefView` retains the
authoritative content reference, media type, and provider kind. To retrieve
the bytes of a known reference:

```ts
const output = run.output;
if (output) {
  const bytes = await client.readBlobBytes(output.blobRef);
  console.log({ bytes: bytes.length, mime: output.mime,
    providerKind: output.providerKind });
}
```

This fragment continues the earlier script. Do not assume those bytes are
plain text: model output can be provider-native JSON or media. Tool and catalog
previews are bounded, while their full bodies remain available by reference.
Use the declared representation when decoding or storing an artifact.

## Inspect a run

An application that shows what a session did can use these reads directly, without
processing events:

| To show | Call |
| --- | --- |
| The sessions for an application | `listSessions`, filtered by `metadata` |
| One session's state | `readSession` |
| Its runs | `listRuns` |
| One run in full | `readRun`, then `runTimeline` |
| A tool's complete output | `readBlobText(call.outputRef)` when `outputPreviewTruncated` is set |

`readRun` returns messages and tool calls as separate lists. `runTimeline`
merges them into one list in the order they happened, joining each tool call to its
result, its status and its timing:

```ts
import { runTimeline } from "@lightspeed-ai/sdk";

for (const item of runTimeline(run)) {
  if (item.type === "tool") {
    console.log(item.name, item.call?.status, item.call?.durationMs);
  } else if (item.type === "message") {
    console.log(item.role, item.entry.text);
  }
}
```

To keep a view current, follow the run with `awaitRun` and read it again when `onEvent`
fires. Events tell you something changed; the run's projection is what to show.

## Serve a browser UI

Keep runtime tokens on the server. The Platform exposes the shared resource API
under `/api/v1` and its application-specific routes under `/api/ui`. Browser
sessions pass authentication, same-origin checks, membership, role and resource
visibility checks before a request reaches the runtime.

The shared routes come from the generated catalog, with an explicit admission
registry. The Platform selects a fixed runtime upstream, replaces identity headers,
and supplies the authenticated actor. Most request bodies stream unchanged; only
operations whose policy reads or rewrites fields require bounded body decoding.
The runtime remains responsible for validating the API payload.

A standalone application's gateway must enforce its own equivalent policy. Use
`HTTP_OPERATIONS` from `@lightspeed-ai/sdk/server` for route and target metadata;
do not expose a full-authority runtime token to the browser. A universe partitions
data but does not constrain a runtime token. For example, blob reads authorize
ownership within the selected universe, not visibility of a particular session.

Allow only reviewed operations, constrain the upstream, enforce browser origin
policy for cookie authentication, and preserve cancellation and backpressure when
forwarding SSE or binary responses. A long `awaitRun` may hold a request open for
up to 30 seconds at a time; account for your host's request-duration limit.

<a id="handle-errors-and-lifecycle-explicitly"></a>

## Handle errors and lifecycle

Successful `call` and helper results contain the declared DTO directly. Binary
read helpers return bytes. The watch iterator carries named SSE events separately
from ordinary responses; list hints do not replace the durable session event log.

`LightspeedApiError` preserves HTTP `status`, `kind`, and the problem `payload`.
Its `data` exposes structured problem details, including `submissionId` and model
default information. Kinds are camelCase, such as `invalidRequest` and `notFound`.
A missing or invalid runtime token produces HTTP 401 with kind `unauthenticated`
before the gateway reads the request body.

`LightspeedTransportError` reports invalid responses or network failures. A
transport failure after submission can leave acceptance unknown; reconcile with
stable IDs before retrying mutations. Your own abort or timeout rejects with the
signal's reason, such as `AbortError` or `TimeoutError`.

Two kinds need particular handling:

| Kind | Meaning | What to do |
| --- | --- | --- |
| `operationPending` | The wait expired while the session may still be holding the command, for example behind a compaction. It is not a failure, and the command can still land. | Retry the same request. Run start and steering put the submission ID in `data`, including one the server allocated, so a retry cannot land twice. Session events report the outcome. |
| `sessionExecutionStopped` | The session is open and its history intact, but its workflow execution has stopped. | Call `session/restart`, or decide to close the session. Nothing restarts it behind the caller. |

An approval decision whose wait runs out reports `status: pending` for that decision
rather than failing the call.

| Action | Method and follow-up |
| --- | --- |
| Cancel queued or active work | `session/runs/cancel`, then observe terminal state. Cancellation cannot undo external effects already performed. |
| Send steering to active work | `session/runs/steer` with a stable `submissionId`; the next model turn consumes accepted input. A parked run holds it without waking. |
| Decide tool approvals | `session/runs/approvals/decide`; inspect per-decision results and resolve all pending approvals. |
| Update session setup | Read the current revision, then `session/config/put` while idle. It replaces the sparse configuration, including feature grants. |
| Finish the conversation | `session/close`; ordinary close requires an idle session. Closure and deletion are separate decisions. |
| Bring back a stopped session | `session/restart`; a run that was executing fails, and a parked run keeps waiting. |

The [API reference](../../../crates/api/contract/api-reference.md) describes
each operation. The client's `OPERATION_INFO` exposes the runtime operation metadata. Use [Workflow tools](workflow-tools.md) when another
durable workflow needs to own or participate in the session.

## Verify the integration

Exercise a normal run, retry the exact same submission, and confirm that both
responses identify the same run. Disconnect the observer and resume from a
saved cursor. Test cancellation and a provider failure, and confirm your
application distinguishes those terminal states from an HTTP timeout.

The [client tests](../../../clients/typescript/test/client.test.ts) demonstrate
transport errors, stable submission IDs, and event following with an injected
fetch implementation. They are useful fixtures for local client development;
the production endpoint and provider path still need an integration test with
the intended deployment.
