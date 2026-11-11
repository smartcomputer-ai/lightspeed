# Build with the API and TypeScript client

An application submits work to a Lightspeed session and observes it until the
run finishes. The session owns the durable conversation and configuration;
the application owns its business operation, the identifiers used to retry
it, and what to do with the result. Stable identifiers let the application
recover from a lost HTTP response without creating duplicate work.

The TypeScript client supplies typed JSON-RPC calls and helpers for starting
runs and waiting for them to finish. Its public types come from the Rust API contract.
Build against this client and the public contract so runtime implementation
changes do not become dependencies of your application.

## Use the Python SDK

The async Python SDK uses the same gateway contract as the TypeScript client.
Install it from a checkout matching your Lightspeed runtime with
`python -m pip install ./clients/python` using Python 3.12 or newer.

The [Python SDK guide](https://github.com/smartcomputer-ai/lightspeed/tree/main/clients/python)
shows how to authenticate, submit a run, and resume from a cursor. The
[Streamlit walkthrough](https://github.com/smartcomputer-ai/lightspeed/tree/main/examples/streamlit-lightspeed)
runs against the local development deployment and pairs a session chat with a
map of the gateway, Temporal, PostgreSQL, VFS, and environments.

## Prepare an endpoint and a profile

For this example, provide an authenticated gateway URL ending in `/rpc` and a
universe key with the `session` method group. This key authenticates directly
to the runtime, independently of a Platform browser login. Follow
[API keys and service access](../access-and-security/api-keys-and-service-access.md)
to configure the client path.

Run this integration on your application server or another trusted client.
A runtime key can call its allowed method groups throughout its universe,
including private sessions when it has the `session` group. Do not embed it
in a publicly distributed frontend; [serve a browser UI](#serve-a-browser-ui) through
a proxy instead. Applications serving people should
authenticate those people and enforce their own access policy before
submitting work with the application's credentials. A key created with
`assertActor` can attribute that work to them: `client.withActor(userId)` returns a
client that records the ID on each request. Attribution does not limit what the key
can do.

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
`npm install @lightspeed-ai/sdk` for the latest release. Supply `LIGHTSPEED_API_URL` and `LIGHTSPEED_API_KEY` through the
application's protected configuration. A deployment key also needs the `universe`
client option; a universe key must leave it out. The example also reads
`ACORN_RELEASE_JOB_ID`, an application-defined operation ID such as
`acorn-1.2-draft-001`. Keep it unchanged when retrying that operation.

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
  apiKey: required("LIGHTSPEED_API_KEY"),
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
it in the Platform. Omitting `access` creates private work; because this key
asserts no user, only admins can see that session through the Platform.
Open the session and inspect `release-notes.md` to verify the workspace change.
The run's final message and a file it wrote are different outputs. Reuse the
session for a follow-up conversation, or create another session ID for
independent work.

## Use helpers or `call`

The client has two layers. Named helpers such as `startSession`, `readRun`, and
`closeSession` cover what most applications need to use sessions, runs, and the resources
they attach. `client.call(method, params)` reaches every method in the API by its name,
including administration such as auth, deployment, and channels, which have no helpers.

A helper and its `call` do the same thing, so prefer the helper when there is one. They
differ in shape: a helper takes the IDs of its target first and everything else in one
options object, and returns the method's data. `call` takes one params object and one
options object, and returns the `AgentApiOutcome` envelope, including any notifications
sent with the call. A few helpers also add behavior: `startRun` and `steerRun` accept
plain text and generate a submission ID, `awaitRun` combines several calls and has no
single method behind it, and the blob helpers handle base64. The
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

Transcription creates no session or run. A direct key needs `blobs/put` for
upload, `transcriptions` for the job, and `session` for subsequent admission.
Use `transcriptions/cancel` to request cancellation; abandoning an HTTP wait
does not cancel the job.

Matching retries with the same requester, idempotency key, audio, and options
rejoin the original job even if the universe default has changed. Reusing the
key with changed input conflicts. This identity lasts for the Temporal
namespace's workflow-history retention period. The resolved model is fixed
at admission. Asserted actors can read and cancel only their own drafts;
direct universe keys retain their method-group authority.

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
application restarts. JSON-RPC request IDs only correlate transport responses;
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

A browser cannot hold an API key, and the runtime gateway does not accept cross-origin
requests. Mount `createRpcProxy` from `@lightspeed-ai/sdk/server` on your own server,
and point the browser's `LightspeedClient` at it. The proxy forwards only the API methods
you list by name, including those behind any helper the browser calls, calls your `authorize` function for each one, and ignores any identity the
browser sends:

```ts
import { LightspeedClient } from "@lightspeed-ai/sdk";
import { createRpcProxy } from "@lightspeed-ai/sdk/server";

declare function getUser(request: Request): Promise<{ id: string } | null>;

export const handler = createRpcProxy({
  client: new LightspeedClient({
    endpoint: process.env.LIGHTSPEED_API_URL!,
    apiKey: process.env.LIGHTSPEED_API_KEY!,
  }),
  methods: ["session/list", "session/read", "session/runs/list", "session/runs/read"],
  authorize: async ({ request }) => {
    const user = await getUser(request);
    if (!user) return { deny: "unauthenticated" };
    return { actor: user.id };
  },
});
```

`authorize` can also return `params` to replace what the browser sent, for example to
force a `metadata` filter onto `session/list`. Treat the params it receives as untrusted,
because nothing has validated them yet. Returning an `actor` needs a key created with
`assertActor`, and records who asked without limiting the key. A denial reaches the
browser as a `LightspeedRpcError` with kind `unauthenticated` or `forbidden`.

A universe is one readable namespace for a key allowed to read it. `blobs/read` takes
only a blob reference and is not tied to a session, so anyone you allow that method can
read any blob in the universe whose reference they hold. If an application must not see
some work, give it a separate universe and key.

Serve the proxy from the same origin as your page, because it sends no CORS headers. It
accepts only POST with a JSON body, which also stops a cross-site form from using a
signed-in user's cookies. A long `awaitRun` holds a request open for up to 30 seconds at
a time, so check your host's limit on request duration. The
[run inspector example](../../../clients/typescript/examples/run-inspector/README.md)
is a small working page that uses all of this.

<a id="handle-errors-and-lifecycle-explicitly"></a>

## Handle errors and lifecycle

Successful `call` results retain the `AgentApiOutcome` envelope:
`outcome.result` contains method data and `outcome.notifications` contains
notifications returned with that call. Helpers return the method data alone. Those notifications do not replace the
durable session event stream.

`LightspeedRpcError` preserves `code`, `message`, `kind`, and structured `data`.
Inspect those fields for conflicts, rejected operations, missing records, or
an environment that is not ready. Every `kind` is camelCase, such as `invalidRequest`
or `notFound`. `LightspeedTransportError` reports failures such as HTTP errors, invalid
responses, or network interruption. Its `transient` flag says the same request may
succeed later, and `cause` keeps the underlying failure. A transport failure after
submission can leave acceptance unknown; reconcile with stable IDs before retrying
mutations. Your own abort or timeout is not a transport failure: the call rejects with
the signal's reason, such as an `AbortError` or `TimeoutError`.

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
each operation. The client's `METHOD_INFO` exposes the same method metadata. Use [Workflow tools](workflow-tools.md) when another
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
