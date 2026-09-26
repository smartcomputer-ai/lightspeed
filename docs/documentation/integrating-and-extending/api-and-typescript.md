# Build with the API and TypeScript client

An application submits work to a Lightspeed session and observes it until the
run finishes. The session owns the durable conversation and configuration;
the application owns its business operation, the identifiers used to retry
it, and what to do with the result. Stable identifiers let the application
recover from a lost HTTP response without creating duplicate work.

The TypeScript client supplies typed JSON-RPC calls and helpers for starting
runs and following events. Its public types come from the Rust API contract.
Build against this client and the public contract so runtime implementation
changes do not become dependencies of your application.

## Prepare an endpoint and a profile

For this example, provide an authenticated gateway URL ending in `/rpc` and a
universe key with the `session` method group. This key authenticates directly
to the runtime, independently of a Platform browser login. Follow
[API keys and service access](../access-and-security/api-keys-and-service-access.md)
to configure the client path.

Run this integration on your application server or another trusted client.
A runtime key can call its allowed method groups throughout its universe,
including private sessions when it has the `session` group. Do not embed it
in a publicly distributed frontend. Applications serving people should
authenticate those people and enforce their own access policy before
submitting work with the application's credentials.

Create the `release-editor` profile and `release-notes` workspace from
[Build your first agent](../getting-started/first-agent.md), including
`changes.md` and a working model connection. The example reuses that setup
to prepare the Acorn 1.2 notes from an application.

Use Node.js 24 or newer and a client version corresponding to your deployed
Lightspeed release. Tagged releases publish `@lightspeed-ai/agent-client`;
pin the release's package version in your application. Inside this repository,
the package is also available as a workspace dependency.

```bash
npm install --save-exact "@lightspeed-ai/agent-client@<release-version>"
```

Replace `<release-version>` with the actual package version before running the
command. Supply `LIGHTSPEED_API_URL` and `LIGHTSPEED_API_KEY` through the
application's protected configuration. The example also reads
`ACORN_RELEASE_JOB_ID`, an application-defined operation ID such as
`acorn-1.2-draft-001`. Keep it unchanged when retrying that operation.

## Submit and observe one task

Save this as `release-notes.mts`. The session and submission IDs
allow the same script to find the same work after a network failure:

```ts
import { LightspeedClient } from "@lightspeed-ai/agent-client";

function required(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`Missing ${name}`);
  return value;
}

const jobId = required("ACORN_RELEASE_JOB_ID");
const client = new LightspeedClient({
  endpoint: required("LIGHTSPEED_API_URL"),
  headers: { authorization: `Bearer ${required("LIGHTSPEED_API_KEY")}` },
});

const started = await client.call("session/start", {
  sessionId: `release-${jobId}`,
  displayName: "Acorn 1.2 release notes",
  access: { visibility: "universe" },
  profile: { kind: "named", profileId: "release-editor" },
  metadata: { application: "release-pipeline", jobId },
});
const sessionId = started.result.session.id;

const accepted = await client.startRun(sessionId, [
  {
    type: "text",
    text: "Read changes.md in the release-notes workspace, write the Acorn 1.2 " +
      "release-notes.md draft, and return a short summary of what you wrote.",
  },
], { submissionId: `draft-${jobId}` });
const runId = accepted.result.run.id;
console.log({ sessionId, runId, status: accepted.result.run.status });

await client.awaitRun(sessionId, runId, {
  signal: AbortSignal.timeout(5 * 60_000),
  onPage(page) {
    if (page.result.gap) {
      throw new Error("Event history has a gap; reconcile this run before continuing");
    }
  },
  onEvent(event) {
    console.log(event.cursor.seq, event.kind.type);
  },
});

const { result } = await client.call("session/runs/read", { sessionId, runId });
if (result.run.status !== "completed") {
  throw new Error(`Run ${runId} ended with status ${result.run.status}`);
}
console.log(result.run.outputText ?? "Completed without a text output");
```

Run it with the configured environment:

```bash
node release-notes.mts
```

The start response means the run was admitted. It can be queued behind
existing work or already running; it does not mean generation finished.
`awaitRun` follows session events until it sees a completed, failed, or canceled
event for the requested run. The final read returns that run's projection,
including its complete visible terminal text in `outputText` when present.

The example creates a session shared with the universe so teammates can inspect
it in the Platform. Omitting `access` creates private work; because this key
asserts no user, only admins can see that session through the Platform.
Open the session and inspect `release-notes.md` to verify the workspace change.
The run's final message and a file it wrote are different outputs. Reuse the
session for a follow-up conversation, or create another session ID for
independent work.

## Keep retry identity with the business operation

Persist the session ID, submission ID, request input/configuration, and returned
run ID with your application's job record. The example derives stable IDs
from that record's job ID. If the response is lost, repeat the same request
with the same identifiers and contents.

`session/start` with an existing ID returns that session. It does not reapply
the creation profile or replace its configuration. Reusing a session ID for a
different business operation can therefore reconnect to old state. Creation
retries can still validate supplied profile/configuration references, so an
invalid or deleted referenced profile can prevent a retry. Once IDs are saved,
read the known session/run directly during recovery instead of unnecessarily
repeating creation.

`session/runs/start` deduplicates by submission ID within the session. The
same ID and source/configuration/terminal notification return the original
run. Reusing the ID with changed inputs is rejected. Allocate a new submission
ID when asking for another run.

The `startRun` helper generates an ID if you omit one, but calling the helper
again generates another ID. That default does not provide retry safety across
application restarts. JSON-RPC request IDs only correlate transport responses;
they do not replace submission IDs. The client does not automatically retry
failed HTTP calls.

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
an `after` cursor. The example omits it to observe the retained stream from
the beginning. If a saved cursor might already be past the target's terminal
event, read the run before waiting again; the helper looks for terminal events
after its cursor and does not independently check current run state.

Check `gap` when reconstructing history. The helper exposes it through
`onPage` but does not implement application recovery for missing events.
`complete` means the requested direction has no more events at the instant
of the read; it does not mean the session is closed.

For a transcript UI, `session/events/read` also supports backward pagination.
Use `nextCursor` as `before` to load older history, and use the initial page's
`headCursor` as `after` for the forward live stream. Keep those two cursor
directions separate. Pages can split a run or tool batch, so retain enough
projection state to join their pieces.

## Read content through the public projection

`session/read` returns current session state and a bounded recent run-summary
page. Follow its run cursor through `session/runs/list` for older summaries.
`session/runs/read` provides one run's detail; unusually large run histories can
exceed its detail ceiling and require event-stream reconstruction instead.

Use `outputText` for visible terminal text. A `ContentRefView` retains the
authoritative content reference, media type, and provider kind. To retrieve
the bytes of a known reference:

```ts
const output = result.run.output;
if (output) {
  const blob = await client.call("blobs/read", { blobRef: output.contentRef });
  const bytes = Buffer.from(blob.result.bytesBase64, "base64");
  console.log({ bytes: bytes.length, mediaType: output.mediaType,
    providerKind: output.providerKind });
}
```

This fragment continues the earlier script. Do not assume those bytes are
plain text: model output can be provider-native JSON or media. Tool and catalog
previews are bounded, while their full bodies remain available by reference.
Use the declared representation when decoding or storing an artifact.

<a id="handle-errors-and-lifecycle-explicitly"></a>

## Handle errors and lifecycle

Successful `call` results retain the `AgentApiOutcome` envelope:
`outcome.result` contains method data and `outcome.notifications` contains
notifications returned with that call. Those notifications do not replace the
durable session event stream.

`LightspeedRpcError` preserves `code`, `message`, `kind`, and structured `data`.
Inspect those fields for conflicts, rejected operations, missing records, or
an environment that is not ready. `LightspeedTransportError` reports failures
such as HTTP errors, invalid responses, or network interruption. A transport
failure after submission can leave acceptance unknown; reconcile with stable
IDs before retrying mutations.

| Action | Method and follow-up |
| --- | --- |
| Cancel queued or active work | `session/runs/cancel`, then observe terminal state. Cancellation cannot undo external effects already performed. |
| Send steering to active work | `session/runs/steer`; the next model turn consumes accepted input. |
| Decide tool approvals | `session/runs/approvals/decide`; inspect per-decision results and resolve all pending approvals. |
| Update session setup | Read the current revision, then `session/config/put` while idle. It replaces the sparse configuration, including feature grants. |
| Finish the conversation | `session/close`; ordinary close requires an idle session. Closure and deletion are separate decisions. |

The [API reference](../../../crates/api/contract/api-reference.md) describes
each operation. The client's generated `rpc` helpers and `METHOD_INFO` expose
the same method metadata. Use [Workflow tools](workflow-tools.md) when another
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
