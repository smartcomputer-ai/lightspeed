# Lightspeed TypeScript SDK

Start and inspect Lightspeed agent sessions from TypeScript. The SDK is a typed
HTTP client generated from the Lightspeed API contract, with helpers for
starting runs, waiting for them to finish, inspecting what they did, and serving a
browser UI.

A runtime token can call every method in every universe, including reading private
sessions, so keep it on your server. The client itself also
runs in a browser; use a same-origin Platform session or [give it a proxy](#serve-a-browser-ui) instead of a runtime token.

## Connect through Platform

Use the deployment origin or `/api/v1` URL with a user or service-user API
key. Runtime resources and native Platform operations share `client.call` and
one credential. Platform derives the principal; omit `actor`.

```ts
import { LightspeedClient } from "@lightspeed-ai/sdk";

const client = new LightspeedClient({
  endpoint: "https://lightspeed.example",
  token: process.env.LIGHTSPEED_PLATFORM_KEY!,
});
const { organizations } = await client.platform.listOrganizations();
const { universe } = await client.platform.createUniverse({
  organizationId: organizations[0]!.id,
  universeId: crypto.randomUUID(), // Keep this ID if retrying creation.
  name: "Experiment",
  handle: "experiment",
});
// The same typed operation is available through the common dispatcher.
const organization = await client.call("platform/organizations/read", {
  organizationId: organizations[0]!.id,
});
```

Creation requires an organization Creator or Admin and a credential that permits
creation. Native helpers under `client.platform` are generated from the same
contract as their routes and types. Each accepts a typed parameter object and
optional `CallOptions`; helpers without required parameters accept no arguments.
Organization IDs remain explicit parameters. Key-management routes require a human
browser or CLI session; API keys cannot mint broader credentials.

`ClientOperationId`, `ClientOperationParams`, and `ClientOperationResult` describe
the composed client. `OperationId` and the corresponding runtime metadata remain
runtime-only for server adapters. `client.initialize()` returns a discriminated
`ClientConnectionResponse`: when `runtime === "platform"`, `platform` contains
the authenticated principal, credential kind, accessible organizations, and restrictions.

Origins, trailing slashes, and deployment base paths normalize consistently:
`https://example.test/team/` becomes `https://example.test/team/api/v1`.
Credential-bearing URLs, queries, fragments, and redirects are refused.
Ordinary construction performs no discovery or implicit universe selection;
set `universe` explicitly for runtime resource calls. A same-origin browser
client can use its Platform session cookies without embedding a key.

## Use a saved CLI connection in Node.js

First save a deployment with `lightspeed connect add`, or sign in with
`lightspeed connect login --url https://lightspeed.example --name work`.
Then import the Node-only factory:

```ts
import { createClientFromConnection } from "@lightspeed-ai/sdk/node";

const client = await createClientFromConnection({ name: "work" });
const { organizations } = await client.platform.listOrganizations();
if (client.organizationId) {
  const serviceUsers = await client.platform.listServiceUsers({
    organizationId: client.organizationId,
  });
}
```

The factory defaults to the CLI's selected connection. `configDir` overrides the
configuration directory; otherwise it uses `LIGHTSPEED_CONFIG_DIR`, then
`XDG_CONFIG_HOME/lightspeed` or `~/.config/lightspeed`. It verifies the saved
server and credential kinds, reads private credential files for each request,
and persists renewal of CLI-owned sessions atomically, including renewals on
denied operations. Imported credential files and API keys are never rewritten.
It neither logs in automatically nor borrows environment tokens or endpoint
overrides. Saved HTTPS requirements and explicit CLI development exceptions apply.

`universe` and `organization` factory options override selections for this client
without editing the saved connection. Changing organizations clears the inherited
universe unless you also supply `universe`. The selected organization is available
as read-only `client.organizationId`; helpers still require explicit organization IDs.
Removing or rebinding the named connection stops subsequent authenticated calls.
This factory is separate from the browser-safe main package entry.

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

You need the gateway's `/api/v1` URL, a
[runtime token](https://ls.bot/docs/access-and-security/api-keys-and-service-access/), and a universe with a default model or a profile to start from.

```ts
import { LightspeedClient } from "@lightspeed-ai/sdk";

const client = new LightspeedClient({
  endpoint: process.env.LIGHTSPEED_API_URL!,
  token: process.env.LIGHTSPEED_API_TOKEN!,
  universe: process.env.LIGHTSPEED_UNIVERSE!,
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
| `endpoint` | Deployment origin or `/api/v1` base URL, for runtime or Platform. |
| `token` | Sent as the bearer credential. |
| `universe` | The universe's UUID or handle; required for universe methods in authenticated mode. |
| `actor` | An opaque user ID for attribution. Any direct runtime token may name an actor. |
| `headers` | Extra headers, or a function returning them for each request. These override the options above. |
| `fetch` | A replacement `fetch`, for tests or custom transports. |

To act for several users from one client, use `client.withActor(userId)`. Attribution
records who asked, but it does not limit what the token can do, so your application
must check each user's access itself.

## Helpers and `call`

The client has two layers, and you can mix them freely:

- **Named helpers** cover all 148 runtime operations and all 26 native Platform operations.
- **`client.call(method, params)`** reaches every method in the API, typed, by its name.

Use named helpers for discoverable, typed arguments, or `call` when selecting an
operation by its stable ID. Authorization is the same for both. The shapes differ:

```ts
// A helper: IDs of the target come first, everything else goes in options, and you
// get the method's data directly.
const { session } = await client.readSession(sessionId, { runLimit: 5 });
await client.closeSession(sessionId, { force: true, signal });

// call: one params object and one options object, returning the same direct DTO.
const result = await client.call("session/read", { sessionId, runLimit: 5 }, { signal });
console.log(result.session);

// Administration also has named helpers.
const universes = await client.listUniverses();
await client.putUniverseExpiry("universe-id", { expiresAtMs: null });
```

Use `watch` for live list changes and `readEvents` for durable session events.
Both layers reject with the same
[errors](#handle-errors), and every option also accepts `signal` and `headers`.

Helper availability describes the client surface; the deployment still decides
which operations your credential may use. Platform exposes permitted runtime
and deployment operations through its policy checks. For organization-qualified
universe creation on Platform, use `client.platform.createUniverse({ organizationId,
... })`; `client.createUniverse(id, options)` addresses the runtime deployment
operation without adding organization ownership.

| Area | Runtime helpers |
| --- | --- |
| initialize | `initialize` |
| Sessions | `startSession`, `readSession`, `listSessions`, `renameSession`, `shareSession`, `restartSession`, `closeSession`, `deleteSession` |
| session/metadata | `putSessionMetadata` |
| session/config | `putSessionConfig` |
| session/runs | `listRuns`, `readRun`, `cancelRun`, `decideApprovals`, `startRun`, `steerRun` |
| session/context | `appendContext`, `compactContext`, `removeContext`, `replaceContext` |
| session/profiles | `applyProfile` |
| session/skills | `listSessionSkills` |
| session/environments | `activateEnvironment`, `deactivateEnvironment` |
| profiles | `createProfile`, `readProfile`, `listProfiles`, `putProfile`, `deleteProfile` |
| models | `listModels`, `readModelDefaults`, `putModelDefaults` |
| Environments | `createEnvironment`, `readEnvironment`, `listEnvironments`, `closeEnvironment`, `updateEnvironment` |
| environments/templates | `listEnvironmentTemplates`, `readEnvironmentTemplate` |
| vfs | `createWorkspace`, `readWorkspace`, `listWorkspaces`, `deleteWorkspace`, `readWorkspaceFile`, `commitSnapshot`, `readSnapshot`, `updateWorkspace` |
| mcp | `readMcpServer`, `listMcpServers`, `putMcpServer`, `discoverMcpServerAuth`, `discoverMcpServerTools`, `deleteMcpServer` |
| Bots | `readBot`, `listBots`, `createBot`, `putBot`, `closeBot`, `deleteBot` |
| bots/events | `listBotEvents`, `admitBotEvent`, `replayBotEvent`, `readBotEvent` |
| session/retention | `putSessionRetention` |
| environments/credentials | `bindEnvironmentCredential`, `listEnvironmentCredentials`, `unbindEnvironmentCredential` |
| environments/external | `createExternalEnvironment` |
| environments/registered | `putRegisteredEnvironment`, `releaseRegisteredEnvironment` |
| environments/ingress | `putEnvironmentIngress` |
| environments/power | `putEnvironmentPower` |
| environments/providers | `listAvailableEnvironmentProviders`, `readAvailableEnvironmentProvider` |
| environments/jobs | `createEnvironmentJobs`, `readEnvironmentJobs`, `cancelEnvironmentJobs` |
| environments/registration-keys | `createEnvironmentRegistrationKey`, `putEnvironmentRegistrationKey`, `readEnvironmentRegistrationKey`, `listEnvironmentRegistrationKeys`, `updateEnvironmentRegistrationKey`, `rotateEnvironmentRegistrationKey`, `revokeEnvironmentRegistrationKey` |
| transcriptions | `startTranscription`, `readTranscription`, `cancelTranscription` |
| blobs | `putBlobs`, `hasBlobs`, `composeBlob`, `readBlobBytes`, `uploadBlob` |
| credentials/grants | `importCredentialGrant`, `leaseCredentialGrant`, `readCredentialGrant`, `listCredentialGrants`, `putCredentialGrant`, `revokeCredentialGrant` |
| credentials/clients | `createCredentialClient`, `readCredentialClient`, `listCredentialClients`, `deleteCredentialClient` |
| credentials/flows | `startCredentialFlow`, `readCredentialFlow` |
| credentials/providers | `createCredentialProvider`, `putCredentialProvider`, `readCredentialProvider`, `listCredentialProviders`, `deleteCredentialProvider` |
| credentials/github | `listGithubInstallations`, `grantGithubInstallation` |
| bots/state | `readBotState` |
| bots/sessions | `rotateBotSession` |
| bots/triggers | `putBotTrigger`, `readBotTrigger`, `listBotTriggers`, `readBotTriggerSecrets`, `deleteBotTrigger` |
| bots/filters | `testBotFilter` |
| deployment/universes | `listDeletedUniverses`, `createUniverse`, `listUniverses`, `readUniverse`, `putUniverseConfiguration`, `putUniverseExpiry`, `putUniverseHandle`, `deleteUniverse` |
| deployment/sessions | `listDeletedSessions`, `purgeSession` |
| deployment/environment-provider-bindings | `listDeploymentEnvironmentProviderBindings` |
| deployment/environment-providers | `putEnvironmentProvider`, `listEnvironmentProviders`, `readEnvironmentProvider`, `deleteEnvironmentProvider`, `putUniverseEnvironmentProviderBinding`, `deleteUniverseEnvironmentProviderBinding` |
| universe | `watch` |
| session/events | `readEvents` |

Native Platform helpers use `client.platform` and typed parameter objects:

| Area | Platform helpers |
| --- | --- |
| session | `readCurrentSession`, `revokeCurrentSession` |
| settings | `listPlatformSettings`, `putPlatformSetting` |
| credentials | `listPersonalApiKeys`, `createPersonalApiKey`, `replacePersonalApiKey`, `revokePersonalApiKey` |
| service-users | `listServiceUserApiKeys`, `createServiceUserApiKey`, `putServiceUserApiKey`, `replaceServiceUserApiKey`, `revokeServiceUserApiKey` |
| organizations | `listOrganizations`, `enrollOrganization`, `createOrganization`, `readOrganization`, `updateOrganization`, `listOrganizationMembers`, `putOrganizationMember`, `removeOrganizationMember`, `listOrganizationApiKeys`, `listServiceUsers`, `createServiceUser`, `updateServiceUser`, `listFederatedCredentials`, `putFederatedCredential`, `deleteFederatedCredential` |
| universes | `adoptUniverse`, `createUniverse`, `readPlatformUniverse`, `updateUniverse`, `listUniverseGrants`, `putUniverseGrant`, `removeUniverseGrant`, `readUniverseAccessPolicy`, `putUniverseAccessPolicy` |

Additional conveniences include `awaitRun`, `readBlobText`, `putBlob`,
`openWatch`, and `openDownload`. They compose operations or expose their streams:

- `startRun` accepts plain text, input items, or a full `RunStartSource`; its options
  include `notifyOnTerminal`. `steerRun` accepts plain text or input items. Both
  generate a submission ID if you give none.
- `readEvents` supports forward reads and backward pagination with `direction`
  and `before`, preserving the complete event-read parameter set.
- `awaitRun` has no single method behind it. It follows events, checks run state, retries
  reads, and returns the finished run.
- `putBlob` sends a JSON batch upload; `readBlobText` and `readBlobBytes` download raw bytes.
- `uploadBlob` uploads bytes or a stream to an expected content digest. `openDownload` returns a streaming response that the caller consumes or cancels.

`OPERATION_INFO` describes every operation. `CLIENT_HELPERS` maps every canonical
runtime helper, generated or handwritten, to its operation;
`PLATFORM_CLIENT_HELPERS` maps the native namespace. The
[API reference](https://github.com/smartcomputer-ai/lightspeed/blob/main/crates/api/contract/api-reference.md)
covers them in full. Types are named by the API, and `OperationResult<"session/read">` and
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

Use the shared HTTP route descriptors on your server. `HTTP_OPERATIONS` from
`@lightspeed-ai/sdk/server` contains each operation's HTTP method, path, access
classification, and parameter bindings. Register those routes in your framework,
authenticate the caller, authorize the URL target, then stream requests to the
runtime with the server's bearer token and actor attribution. The runtime validates
request bodies; decode them in the proxy only when a policy needs to inspect or
rewrite a field.

`prepareHttpRequest(operation, params, { endpoint, universe })` constructs a resource
URL and partitions logical parameters between its query and body. Both SDKs use
the same generated contract. Browser clients use a client pointed at your proxy's
base URL with the selected `universe`.

The [run inspector example](examples/run-inspector/README.md) demonstrates a small
route allowlist, application login, server-owned credentials and streamed downloads.
Platform adds its organization and role policies around these shared routes.

## Handle errors

| Error | Meaning |
| --- | --- |
| `LightspeedApiError` | The server answered with an error. Check `kind`, such as `notFound`, `conflict` or `sessionExecutionStopped`, and `data`. A missing or invalid runtime token is `unauthenticated`, which the gateway answers with HTTP 401 before reading the request. |
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

The [cross-SDK live validation](../python/README.md#develop-this-package) uses the
real Platform, runtime and CLI to verify saved sessions/API keys, JSON and binary
requests, scope restrictions and revocation. It requires dedicated fixtures and
revokes their credentials after the check.

## License

Apache-2.0

HTTP paths, query encoding, and response types are generated from `http.json`. Universe requests require the `universe` client option. Discover a single-mode runtime's default with `initialize()` and use its `defaultUniverse` when creating the scoped client. Errors expose an HTTP `status` and structured `kind`; `payload` retains the complete problem document.
