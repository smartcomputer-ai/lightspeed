# Build with the API and TypeScript client

An application submits work to a Lightspeed session, follows the run, and uses
its result. Lightspeed keeps the session and run history. Your application
keeps the job ID and the Lightspeed IDs it needs to recover that work if a
response is lost.

The TypeScript and Python SDKs use the same resource HTTP API. This walkthrough
uses TypeScript and continues the release-notes example from
[Build your first agent](../getting-started/first-agent.md).
See the [Python SDK guide](../../../clients/python/README.md) for Python examples.

## Platform access

Connect to your Platform origin or its `/api/v1` base URL. Use a service-user
API key for an unattended job or a personal API key for work under your own
account. Platform checks the identity's current permissions, the key's
restrictions, and the target resource. Select a universe UUID or handle on
the client. Platform supplies the actor identity from the credential.

A role determines which actions the identity can perform. A key can narrow
those actions to particular organizations, universes, or operation IDs. For
example, a service user with Contributor access can start sessions from a saved
profile. The same user's read-only key can inspect visible sessions but cannot
start one. See [People and roles](../access-and-security/people-and-roles.md)
and [API keys and service access](../access-and-security/api-keys-and-service-access.md).

A trusted infrastructure service can connect directly to a private runtime.
A runtime token has full authority across the deployment. Keep it on the
server. If your application makes direct runtime calls on behalf of people,
enforce their access in your application. Any runtime token can set an actor
for attribution. See [Serve a browser UI](#serve-a-browser-ui).

## Prepare an endpoint and a profile

Set up the `release-editor` profile and the `release-notes` workspace from
[Build your first agent](../getting-started/first-agent.md). Confirm that the
profile can write to the workspace at `/workspace`.

Give the service user at least Contributor access to the universe and
create a key for it. If you use an operation allowlist on the key, include
`session/start`, `session/runs/start`, `session/events/read`, `session/read`, and
`session/runs/read` for the walkthrough.

Use Node.js 24 or newer. Pin the SDK version that matches your deployment,
replacing `<release-version>` with its version.

```bash
npm install --save-exact "@lightspeed-ai/sdk@<release-version>"
```

Supply `LIGHTSPEED_API_URL`, `LIGHTSPEED_API_TOKEN`, and `LIGHTSPEED_UNIVERSE`
through your application's protected configuration. For this example they
hold the Platform URL, API key, and universe UUID or handle. Set
`ACORN_RELEASE_JOB_ID` to an application job identifier such as
`acorn-1.2-draft-001`. Use the same identifier every time you retry that job.

## Submit and observe one task

Save this as `release-notes.mts`.

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
  profile: { kind: "named", profileId: "release-editor" },
});

const { run: started } = await client.startRun(
  session.id,
  "Read /workspace/changes.md, write the Acorn 1.2 release notes to " +
    "/workspace/release-notes.md, and return a short summary.",
  { submissionId: `draft-${jobId}` },
);
console.log({ sessionId: session.id, runId: started.id });

const { run } = await client.awaitRun(session.id, started.id, {
  signal: AbortSignal.timeout(5 * 60_000),
  onEvent(event) {
    console.log(event.cursor.seq, event.kind.type);
  },
});

if (run.status !== "completed") {
  throw new Error(`Run ${run.id} ${run.status}: ${run.failure?.message ?? "no details"}`);
}
console.log(run.outputText ?? "Completed without a text output");
```

Run `node release-notes.mts`. The start response confirms that Lightspeed has
accepted the run. `awaitRun` waits for a completed, failed, or cancelled
result. The timeout ends the client's wait. To stop running work, call
`cancelRun`.

Open `release-notes.md` in the workspace to verify the file. The session starts
unshared and belongs to the service user. A universe Owner can inspect its run
in Platform. Anyone with access to the workspace can inspect the file there.
See [Private and shared work](../access-and-security/private-and-shared-work.md).

## Keep retry identity with the business operation

Save the session ID, submission ID, request contents, and returned run ID with
the application job. If a run request's response is lost, resubmit the same
request with the same submission ID. While Lightspeed remembers that
submission, it returns the original run and rejects changed inputs under the
same ID. Call `readRun` once you know the run ID. The run-submission cache is
bounded, so use the saved run ID for recovery after long delays.

A repeated session ID identifies the existing session. To change its setup,
update that session or use a new ID. `startRun` and `steerRun` generate
submission IDs when omitted. Pass your own IDs when work must survive a client
restart. `awaitRun` retries its reads; application code owns mutation retries.

## Reuse a saved CLI connection

For a local script, sign in with the CLI and select an organization and
universe.

```bash
lightspeed connect login --url https://lightspeed.example --name work
lightspeed platform organization use ORGANIZATION_ID
lightspeed universe use my-universe
```

Use the saved connection from Node.js.

```ts
import { createClientFromConnection } from "@lightspeed-ai/sdk/node";

const client = await createClientFromConnection({ name: "work" });
const profiles = await client.listProfiles();
console.log(profiles);
```

Or use the asynchronous Python client.

```python
import asyncio
from lightspeed_ai import AsyncLightspeedClient

async def main():
    async with await AsyncLightspeedClient.from_connection("work") as client:
        print(await client.call("profiles/list", {}))

asyncio.run(main())
```

Omit the connection name to use the current CLI selection. The factories use
the saved endpoint, resource selections, and credential file. They renew
CLI-owned sessions while the server allows renewal. If the login expires or is
revoked, run `lightspeed connect login` again. See
[Connect to Platform](../using-lightspeed/cli.md#connect-to-platform).

## Resource HTTP API

The same operations can be called over HTTP. This example reads connection
information and lists sessions through Platform. If the key has an operation
allowlist, grant it `initialize` and `session/list` for these calls.

```bash
curl --fail-with-body \
  -H "Authorization: Bearer $LIGHTSPEED_API_TOKEN" \
  "$LIGHTSPEED_API_URL/api/v1/connection"

curl --fail-with-body \
  -H "Authorization: Bearer $LIGHTSPEED_API_TOKEN" \
  "$LIGHTSPEED_API_URL/api/v1/universes/$LIGHTSPEED_UNIVERSE/sessions?limit=20"
```

Set `LIGHTSPEED_API_URL` to the Platform origin for these commands. The universe
is part of the route. Put path identifiers only in the URL, and use the
operation's declared encoding for any query filters or cursors. Successful
JSON responses contain the declared record. API failures use an HTTP error
status and a problem document with a stable `kind`.

Consult the [API reference](../../../crates/api/contract/api-reference.md)
for runtime routes, request fields, and results. The
[OpenAPI document](../../../crates/api/contract/openapi.json) describes the
same resource paths. Platform serves the routes it authorizes and exposes its
own organization, service-user, grant, and key operations under
`/api/v1/platform`. See the
[Platform SDK operations](../../../clients/typescript/README.md#helpers-and-call).

## Use helpers or `call`

Use named SDK helpers when you know the operation, or
`client.call(operation, params)` when selecting it by ID. Both use the same
credential and permissions.

```ts
const { session } = await client.readSession(sessionId, { runLimit: 5 });
const result = await client.call("session/read", { sessionId, runLimit: 5 });
```

Runtime helpers accept target IDs followed by operation options. Platform
helpers live under `client.platform` and take a parameter object. Pass the
organization ID explicitly, even when the saved CLI connection has selected it.

```ts
const { organization } = await client.platform.readOrganization({ organizationId });
```

Use `client.platform.createUniverse` when creating a universe in a Platform
organization. Creation requires an eligible organization role, a credential
that permits creation, and room under the applicable universe limit. The
runtime's deployment operation is for creating universes directly on a
private runtime.

## Follow events and reconnect

Use `readEvents(sessionId, { after, limit, waitMs })` for paginated session
history and forward long polling. Save the continuation cursor after processing
a page. On a restart, resume after that cursor and make repeated processing
safe in your application. An empty page means there is nothing new to read yet.
Use the run's status to determine whether it finished.

`awaitRun` accepts a saved `after` cursor. Its `onEvent` callback receives
events for the requested run, and `onPage` receives every event on each
session page. For a transcript, page backward with `before` and
`nextCursor`, then follow live events from the initial page's `headCursor`.

<a id="read-content-through-the-public-projection"></a>

## Inspect a run

Use `readSession` for session state and recent runs, `listRuns` for older
summaries, and `readRun` for an individual run. To show messages and tool
calls together, pass that run to `runTimeline`.

```ts
import { runTimeline } from "@lightspeed-ai/sdk";

const { run } = await client.readRun(sessionId, runId);
for (const item of runTimeline(run)) {
  if (item.type === "message") console.log(item.role, item.entry.text);
  if (item.type === "tool") console.log(item.name, item.call?.status);
}
```

Use `outputText` for visible terminal text. For a full result behind a content
reference, call `readBlobBytes` with its `blobRef` and decode the bytes according
to their declared representation. A tool result can expose an `outputRef` when
its preview is truncated. Read a known UTF-8 result with `readBlobText`.
Use `readEvents` for history omitted from a bounded run view.

## Build transcript authors and times

`transcriptMetadata(sessionId, events)` returns author and timestamp information
keyed by context entry ID. Pass the union of loaded history and live events.
Recompute when more events arrive, since an older page can supply the original
acceptance time of a message that waited in a queue. `transcriptAuthor` applies
the same author rule to a single entry. Resolve actor IDs to names in your
application. Treat message origin as display metadata.

## Keep session and resource lists current

List calls return the current session summaries, bots, environments, or
workspaces. Use `watch` in either SDK, or request
`GET /api/v1/universes/{universe}/events` with `Accept: text/event-stream`, to
receive changes for those lists.

Update rows by ID and refetch visible lists on `watch/reset`, including the
reset at connection start. Use the same filters as your ordinary list reads.
The watch has no replay cursor, so reconnect and refresh if the stream ends.
Continue using session event pages for durable transcript history.

## Transcribe audio before submitting session input

1. Upload the audio to the universe's content store with `blobs/put`.
2. Call `transcriptions/start` with a stable `idempotencyKey` and an `audio`
   object with `blobRef`, `mime`, and `name`. Use the universe's
   **Speech-to-text** default or supply an `openai:audio-transcriptions` model.
3. Save the `transcriptionId` and poll `transcriptions/read`. A succeeded job
   supplies `text` and `transcriptRef`. Failed jobs can include failure details.
4. Submit the transcript as text or `textRef`. When you submit unchanged
   text, an optional `provenanceRef` can point to the original audio blob.

On Platform, transcription drafts can be read or cancelled only by the
identity that created them. Call `transcriptions/cancel` to stop a job.
Unsubmitted audio and transcripts can expire. Submit or preserve any results
you need to keep.

## Serve a browser UI

A browser app served from the Platform origin can use a signed-in Platform
session and the same `/api/v1` resource routes. For a standalone app, keep the
runtime token on your server and authorize callers and resource access there.

The TypeScript server entry point `@lightspeed-ai/sdk/server` exports
`HTTP_OPERATIONS` with HTTP methods, paths, and parameter bindings. Use an
operation allowlist and a fixed upstream, and preserve cancellation for
event streams and binary transfers. The
[run inspector example](../../../clients/typescript/examples/run-inspector/README.md)
shows a server-owned credential, an application login, and streamed downloads.

<a id="handle-errors-and-lifecycle-explicitly"></a>

## Handle errors and lifecycle

`LightspeedApiError` exposes the HTTP `status`, stable `kind`, and structured
`data`. `LightspeedTransportError` means the client could not use a response or
reach the server. If that happens during a write, check the known resource or
retry with the original identity before submitting new work.

| Result | What to do |
| --- | --- |
| `unauthenticated` | Check that the credential is valid for the endpoint. |
| `forbidden` or `notFound` | Check the selected universe, the identity's permissions, the key's restrictions, and visibility of the resource. |
| `operationPending` | Keep the request and retry identity. The command can still be accepted. For run start or steering, use the `submissionId` returned in the error data. |
| `sessionExecutionStopped` | Read the stored session and decide whether to call `restartSession` or close it. |
| Aborted wait or timeout | Resume observation by run ID, or explicitly cancel the run. |

## Verify the integration

Run the example twice with the same job ID and confirm both attempts identify
the same run. Stop the observer and reconnect from a saved cursor. Exercise
cancellation and a failed run so your application can distinguish a terminal
result from a lost connection.

Use [Workflow tools](workflow-tools.md) when another durable workflow needs
to own or participate in a Lightspeed session.
