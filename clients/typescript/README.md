# Lightspeed TypeScript SDK

Start and inspect Lightspeed agent sessions from TypeScript. The SDK is a typed
JSON-RPC client generated from the Lightspeed API contract, with helpers for
starting runs, waiting for them to finish, inspecting what they did, and serving a
browser UI.

An API key with the `session` group can read and control every session in its
universe, including private ones, so keep it on your server. The client itself also
runs in a browser; [give it a proxy](#serve-a-browser-ui) instead of a key.

## Install

Requires Node.js 24 or newer.

```bash
npm install @lightspeed-ai/sdk
```

We recommend pinning the version that matches your Lightspeed release, because the
client's types follow that release's API:

```bash
npm install --save-exact @lightspeed-ai/sdk@<release-version>
```

## Start a session and run a task

You need the gateway's `/rpc` URL, an
[API key](https://ls.bot/docs/access-and-security/api-keys-and-service-access/) with the
`session` group, and a universe with a default model or a profile to start from.

```ts
import { LightspeedClient } from "@lightspeed-ai/sdk";

const client = new LightspeedClient({
  endpoint: process.env.LIGHTSPEED_API_URL!,
  apiKey: process.env.LIGHTSPEED_API_KEY!,
});

const { session } = await client.startSession({
  displayName: "Repository summary",
  metadata: { application: "my-app" },
});

const started = await client.startRun(session.id, "Summarize this repository.");

const { run } = await client.awaitRun(session.id, started.run.id, {
  signal: AbortSignal.timeout(5 * 60_000),
  onEvent: (event) => console.log(event.kind.type),
});

if (run.status === "completed") {
  console.log(run.outputText);
} else {
  console.error(run.status, run.failure?.kind, run.failure?.message);
}
```

Giving up on the wait does not cancel the run. It keeps going, and you can read or
wait for it again later. To cancel it, call `cancelRun`.

For work that must survive a lost response or a restart, supply your own `sessionId`
and `submissionId` so a retry finds the same session and run. See
[Build with the API and TypeScript client](https://ls.bot/docs/integrating-and-extending/api-and-typescript/).

## Connect

| Option | Use |
| --- | --- |
| `endpoint` | The gateway's `/rpc` URL. |
| `apiKey` | Sent as the bearer credential. |
| `universe` | The universe's UUID or handle, for a deployment key only. A universe key must leave it out. |
| `actor` | An opaque user ID to attribute requests to. The key must be created with `assertActor`. |
| `headers` | Extra headers, or a function returning them for each request. These override the options above. |
| `fetch` | A replacement `fetch`, for tests or custom transports. |

To act for several users from one client, use `client.withActor(userId)`. Attribution
records who asked, but it does not limit what the key can do, so your application
must check each user's access itself.

## Helpers and `call`

The client has two layers, and you can mix them freely:

- **Named helpers** such as `client.readSession(id)` cover what most applications need.
- **`client.call(method, params)`** reaches every method in the API, typed, by its name.

A helper and its `call` do the same thing. Prefer the helper when there is one, and use
`call` for everything else, mainly administration. The two differ in shape:

```ts
// A helper: IDs of the target come first, everything else goes in options, and you
// get the method's data directly.
const { session } = await client.readSession(sessionId, { runLimit: 5 });
await client.closeSession(sessionId, { force: true, signal });

// call: one params object and one options object, and you get the whole envelope.
const outcome = await client.call("session/read", { sessionId, runLimit: 5 }, { signal });
console.log(outcome.result.session, outcome.notifications);

// Administration has no helper.
const keys = await client.call("deployment/api-keys/list", {});
```

Use `call` if you need the `notifications` returned with a method; helpers leave them out.
The durable session event stream is the same either way. Both layers reject with the same
[errors](#handle-errors), and every option also accepts `signal` and `headers`.

| Area | Helpers |
| --- | --- |
| Connection | `initialize` |
| Sessions | `startSession`, `readSession`, `listSessions`, `renameSession`, `putSessionMetadata`, `putSessionConfig`, `shareSession`, `restartSession`, `closeSession`, `deleteSession` |
| Runs | `startRun`, `awaitRun`, `readRun`, `listRuns`, `cancelRun`, `steerRun`, `decideApprovals`, `readEvents` |
| Session setup | `appendContext`, `compactContext`, `applyProfile`, `listSessionSkills`, `activateEnvironment`, `deactivateEnvironment` |
| Content | `putBlob`, `readBlobText`, `readBlobBytes` |
| Profiles | `createProfile`, `readProfile`, `listProfiles`, `putProfile`, `deleteProfile` |
| Models | `listModels`, `readModelDefaults` |
| Environments | `createEnvironment`, `readEnvironment`, `listEnvironments`, `closeEnvironment`, `listEnvironmentTemplates` |
| Workspaces | `createWorkspace`, `readWorkspace`, `listWorkspaces`, `deleteWorkspace`, `readWorkspaceFile` |
| MCP servers | `readMcpServer`, `listMcpServers` |
| Bots | `readBot`, `listBots`, `listBotEvents`, `admitBotEvent` |

Everything else stays on `call`: auth grants and providers, deployment and universe
management, channels, registration keys, transcription, and the remaining bot, MCP, and
environment operations. A few helpers do more than wrap one method:

- `startRun` and `steerRun` accept plain text and generate a submission ID if you give none.
- `awaitRun` has no single method behind it. It follows events, checks run state, retries
  reads, and returns the finished run.
- `putBlob`, `readBlobText`, and `readBlobBytes` handle the base64 encoding.

`METHOD_INFO` describes every method, `CLIENT_HELPERS` maps each generated helper to its
method, and the
[API reference](https://github.com/smartcomputer-ai/lightspeed/blob/main/crates/api/contract/api-reference.md)
covers them in full. Types are named by the API, and `MethodData<"session/read">` and
`SessionEventKindOf<"runFailed">` cover the common cases.

## Wait for a run

`awaitRun` returns `{ run, cursor }` once the run has finished, where `run` is the same
view `readRun` returns.

- `onEvent` receives the run's own events. `onPage` receives every event page for the
  session, and `heartbeat` receives the cursor after each page.
- Pass `after` with a saved `cursor` to resume. Without it, the wait reads the session
  from its beginning.
- A run that has already finished returns at once, whatever the cursor.
- Reads are repeated through network failures and stalled connections for about 30
  seconds before the wait rejects.

## Inspect a run

`readRun` returns a run's messages and tool calls as separate lists. `runTimeline` merges
them into one list in the order they happened, with each tool call joined to its result:

```ts
import { runTimeline } from "@lightspeed-ai/sdk";

const { run } = await client.readRun(sessionId, runId);

for (const item of runTimeline(run)) {
  if (item.type === "message") {
    console.log(item.role, item.entry.text);
  } else if (item.type === "tool") {
    console.log(item.name, item.call?.status, item.call?.durationMs);
    // Output here is a bounded preview. The whole of it is a blob.
    if (item.call?.outputPreviewTruncated && item.call.outputRef) {
      console.log(await client.readBlobText(item.call.outputRef));
    }
  }
}
```

`readBlobText` rejects with a `TypeError` if the blob is not UTF-8. `readBlobBytes`
returns the raw bytes. Both work in a browser.

To keep a view current, follow the run with `awaitRun` and read it again when
`onEvent` fires, rather than building the view from events yourself.

## Derive transcript authors and times

`transcriptMetadata(sessionId, events)` returns message metadata keyed by context
entry ID. Supply the union of loaded history and live events; order and overlapping
pages do not matter. Rerun it when that union grows, since older pages can supply
an input's acceptance event. It performs no requests and changes no input.

```ts
import { transcriptMetadata } from "@lightspeed-ai/sdk";

// Compute once for the loaded event window, then reuse for every message.
const metadataByEntryId = transcriptMetadata(sessionId, loadedEvents);
const metadata = metadataByEntryId.get(entry.id);
```

The result contains `sentAtMs`, `timeSource`, `author`, and `requestedBy`. Input
uses its run or steering acceptance time; other messages use the earliest loaded
context event carrying the entry. These are existing server event times, not a
new timestamp or a read receipt. A queued input without an applied entry has no
entry ID to look up yet. This helper supplies metadata, not transcript ordering
or rendering.

An actor author includes its opaque `id` and a `source`: `requestedBy` is the
recorded authenticated attribution; `origin` is an unverified `user:<id>` display
claim. `origin=event` produces `{ kind: "event" }` for bot deliveries. Assistant
messages have no user author. The full acceptance attribution remains in
`requestedBy`, including service keys and internal work.

`transcriptAuthor(origin, requestedBy?)` exposes the same author rule separately,
for example for a user-role session-list preview. Resolve names and decide labels
in your application. See [session view data](../../docs/documentation/integrating-and-extending/api-and-typescript.md#build-transcript-authors-and-times)
for Platform integration and live list updates.

## Serve a browser UI

Mount `createRpcProxy` on your server. It holds the key, and the browser uses an
ordinary client pointed at it:

```ts
// On your server. This import is separate so it stays out of browser bundles.
import { LightspeedClient } from "@lightspeed-ai/sdk";
import { createRpcProxy } from "@lightspeed-ai/sdk/server";

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

```ts
// In the browser.
const client = new LightspeedClient({ endpoint: new URL("/api/lightspeed", location.origin) });
```

The handler takes a standard `Request` and returns a `Response`, so it fits Next, Hono,
and similar frameworks.

- **Only listed methods are forwarded.** Everything else is refused before `authorize`
  runs, so add methods as your UI needs them. List them by API name, because that is what
  reaches the proxy even when the browser calls a helper: `listSessions` needs
  `session/list`, and `CLIENT_HELPERS` has the full mapping. Mutations such as
  `session/runs/cancel` are worth checking per user.
- **`authorize` decides every call.** It can deny, name the `actor`, or return `params`
  to replace what the browser sent, for example to force a `metadata` filter onto
  `session/list`. The params it receives are untrusted and not yet validated.
- **The browser supplies no identity.** Its key, universe, and actor headers are ignored.
  Only POST with a JSON body is accepted, and no CORS headers are sent, so serve the
  proxy from the same origin as your page.
- **Refusals look like any other API error.** A denial reaches the browser's client as a
  `LightspeedRpcError` with kind `unauthenticated` or `forbidden`.

A universe is one readable namespace for a key allowed to read it. `blobs/read` takes
only a blob reference and is not tied to a session, so anyone you allow that method can
read any blob in the universe whose reference they hold. If an application must not see
some work, give it a separate universe and key.

A long `awaitRun` from the browser holds a request open for up to 30 seconds at a time.
Some serverless hosts and proxies cut these off; the wait retries, but check your host's
limit. The [run inspector example](https://github.com/smartcomputer-ai/lightspeed/tree/main/clients/typescript/examples/run-inspector)
puts all of this together in a small working page.

## Handle errors

| Error | Meaning |
| --- | --- |
| `LightspeedRpcError` | The server answered with an error. Check `kind`, such as `notFound`, `conflict` or `sessionExecutionStopped`, and `data`. A missing or invalid API key is `unauthenticated`, which the gateway answers with HTTP 401 before reading the request. |
| `LightspeedTransportError` | The request failed or the response was unusable. `status` and `body` are set when there was a response, `cause` keeps the underlying failure, and `transient` says the same request may succeed later. |
| The signal's reason | You aborted, or your timeout passed. Check `error.name` for `AbortError` or `TimeoutError`. |

Only `awaitRun` retries, and only its own reads. After a transport failure on a mutation, the request may
still have landed, so repeat it with the same IDs rather than new ones.

## Write workflow code

Temporal workflows that receive tool calls from an integrated session import
`@lightspeed-ai/sdk/workflow`. It has the generated types and constants, plus helpers for
parsing, IDs, recipes, and replies. It is safe to use in Temporal's workflow sandbox and
does not depend on any Temporal package. See
[Workflow tools](https://ls.bot/docs/integrating-and-extending/workflow-tools/).

## Develop this package

The types, method map, and most helpers are generated from the committed contract in
`crates/api/contract/`, so don't edit `src/generated/` by hand. To give a method a helper,
add it to [`scripts/helpers.mjs`](scripts/helpers.mjs) and to the table above; a helper
that needs behavior of its own goes in `src/client.ts`. Regenerate and check with:

```bash
npm install
npm run check --workspace @lightspeed-ai/sdk
```

`npm run check:generated` regenerates the types and packaged schemas, and fails if the
committed output is stale.

## License

Apache-2.0
