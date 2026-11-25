# Lightspeed HTTP API Reference

Generated from the Rust operation catalog. `openapi.json` defines the HTTP request and response shapes; `http.json` supplies the same route, policy and input-partition metadata to generated adapters.

Calls use resource URLs under `/api/v1` and return their response DTO directly. Errors use `application/problem+json` with a stable `kind` and typed extensions. Universe selection comes from the URL; a caller-supplied `x-lightspeed-universe` header is refused.

Every configured runtime token can call every operation. API family, role and target metadata support external gates such as Platform; the runtime does not enforce caller permissions.

SDK logical inputs and response DTOs use the draft-07 `api.schema.json` bundle.

Path identifiers are omitted from JSON bodies, including nested identifiers. Query scalars use their ordinary spelling, arrays repeat the key, and structured values use JSON encoding as declared in OpenAPI. Unknown query/body fields and duplicate scalar query keys are refused.

## Connection operations

### `initialize`

**Inspect the runtime connection**

Returns runtime identity, HTTP contract revision and configured-token attribution before selecting a universe. Requires authentication in authenticated mode; universe target headers are refused.

- HTTP: `GET /api/v1/connection`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: none
- Access: `{"kind":"connection"}`
- Group: `none`
- Role: `none`


## Universe operations

### `universe/watch`

**Watch universe list changes**

Requires Accept: text/event-stream. Emits list rows after a short batching window, starting with watch/reset. Refetch on reset and periodically: signals can be lost. Ends after 15 minutes; reconnect to reauthenticate. Includes sub-agents; visibleTo restricts session access as on session/list.

- HTTP: `GET /api/v1/universes/{universe}/events`
- Success: `200`
- Transfer: `sse`
- Query: `kinds`, `visibleTo`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `none`
- Role: `viewer`
- Events: named SSE events with params-only JSON data; `watch/end` ends the stream. Reconnect and refetch; no replay cursor is promised.

### `vfs/workspaces/files/read`

**Read a workspace file**

Reads bytes at a path in the current workspace head.

- HTTP: `GET /api/v1/universes/{universe}/vfs/workspaces/{workspaceId}/files/content`
- Success: `200`
- Transfer: `binaryDownload`
- Query: `path`
- Path input fields: `workspaceId` → `workspaceId`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `vfs`
- Role: `viewer`

### `session/start`

**Start a session**

Creates a session, unshared unless access says universe, with optional config, profile and immutable integration. Explicit values override profile metadata and retention; the default environment attachment becomes active. Retrying an id returns it and restarts a stopped execution from its log, unless integrated. Failed setup closes a server-allocated id.

- HTTP: `POST /api/v1/universes/{universe}/sessions`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: JSON object: `access`, `config`, `deleteAfterCloseMs`, `displayName`, `integration`, `metadata`, `profile`, `sessionId`
- Access: `{"action":"createSession","kind":"universe"}`
- Group: `session`
- Role: `contributor`

### `session/read`

**Read a session**

Returns stored state plus a bounded newest-first run-summary page, and needs no worker. An open session whose execution has stopped reports executionFailure. Follow nextRunCursor with session/runs/list when hasOlderRuns is true; use session/events/read for the transcript.

- HTTP: `GET /api/v1/universes/{universe}/sessions/{sessionId}`
- Success: `200`
- Transfer: `json`
- Query: `runLimit`
- Path input fields: `sessionId` → `sessionId`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `session`
- Role: `viewer`

### `session/list`

**List sessions**

Returns a cursor-paginated summary list ordered by most recent update, optionally narrowed by filters such as visibleTo, which keeps sessions whose root is shared with the universe or was created by that actor. An unknown parameter is refused. Pages may shift while sessions are changing.

- HTTP: `GET /api/v1/universes/{universe}/sessions`
- Success: `200`
- Transfer: `json`
- Query: `closed`, `cursor`, `lifecycleControlled`, `limit`, `metadata`, `parent`, `subagent`, `trees`, `visibleTo`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `session`
- Role: `viewer`

### `session/config/put`

**Replace session configuration**

Replaces the complete sparse config while the session is idle. Use the current config revision for safe read-modify-write; omitted features are revoked, an omitted model preserves the current model, and an identical document is a no-op. A newly attached MCP server must be usable; a held one that has gone away only drops out of later runs.

- HTTP: `PUT /api/v1/universes/{universe}/sessions/{sessionId}/config`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `sessionId` → `sessionId`
- Request body: JSON object: `config`, `expectedConfigRevision`
- Access: `{"action":"configureSession","kind":"universe"}`
- Group: `session`
- Role: `operator`

### `session/rename`

**Rename a session**

Sets the display name, or clears it when displayName is omitted.

- HTTP: `PUT /api/v1/universes/{universe}/sessions/{sessionId}/name`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `sessionId` → `sessionId`
- Request body: JSON object: `displayName`
- Access: `{"action":"controlSession","kind":"universe"}`
- Group: `session`
- Role: `contributor`

### `session/metadata/put`

**Replace session metadata**

Replaces the complete descriptive key/value map (bounded like session/start); an omitted or empty map clears it. Record-only: the event log and updatedAtMs are untouched.

- HTTP: `PUT /api/v1/universes/{universe}/sessions/{sessionId}/metadata`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `sessionId` → `sessionId`
- Request body: JSON object: `metadata`
- Access: `{"action":"configureSession","kind":"universe"}`
- Group: `session`
- Role: `operator`

### `session/retention/put`

**Replace session retention**

Sets the positive close-relative automatic soft-deletion duration on a retention root, or clears it with null. Delegated children inherit the root policy and cannot override it. Retention does not permanently delete stored history.

- HTTP: `PUT /api/v1/universes/{universe}/sessions/{sessionId}/retention`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `sessionId` → `sessionId`
- Request body: JSON object: `deleteAfterCloseMs`
- Access: `{"action":"setSessionRetention","kind":"universe"}`
- Group: `session/delete`
- Role: `owner`

### `session/restart`

**Restart a stopped session**

Starts a new workflow execution for an open session whose last one stopped, from its log. A run that was not parked fails, a parked run keeps waiting, and queued runs start afterwards. A running session is left alone. Refused for a closed session and for one with no run history.

- HTTP: `POST /api/v1/universes/{universe}/sessions/{sessionId}/restart`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `sessionId` → `sessionId`
- Request body: empty JSON object or no body
- Access: `{"action":"configureSession","kind":"universe"}`
- Group: `session`
- Role: `operator`

### `session/close`

**Close a session**

Closes an idle session and detaches its environment bindings. Without force, active work of any kind, including a pending detached promise, rejects the call. Force mode cancels active work, drops queued runs, and can recover a session whose workflow is unavailable.

- HTTP: `POST /api/v1/universes/{universe}/sessions/{sessionId}/close`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `sessionId` → `sessionId`
- Request body: JSON object: `force`
- Access: `{"action":"closeSession","kind":"universe"}`
- Group: `session`
- Role: `contributor`

### `session/delete`

**Delete closed sessions**

Permanently deletes a closed retention-tree leaf, or its closed delegated-child subtree when cascade is true. Set softDelete to true to hide sessions while retaining records and history until a deployment administrator purges them. Prior soft deletion is not required for permanent deletion.

- HTTP: `DELETE /api/v1/universes/{universe}/sessions/{sessionId}`
- Success: `200`
- Transfer: `json`
- Query: `cascade`, `sharedOnly`, `softDelete`
- Path input fields: `sessionId` → `sessionId`
- Request body: none
- Access: `{"action":"deleteSession","kind":"universe"}`
- Group: `session/delete`
- Role: `operator`

### `session/share`

**Share a session with the universe**

Moves an unshared root session to universe visibility, one way; its delegated children follow it. Refused on a bot's session, a delegated child, and a session already shared. Core applies it for any caller of the method; who may share is the caller's gate's decision.

- HTTP: `POST /api/v1/universes/{universe}/sessions/{sessionId}/share`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `sessionId` → `sessionId`
- Request body: empty JSON object or no body
- Access: `{"action":"shareSession","kind":"universe"}`
- Group: `session`
- Role: `contributor`

### `session/events/read`

**Read the session event stream**

Returns chronological events. Forward (default) follows after and supports long-polling. Backward reads the latest window below before (or the head); pass nextCursor as before until complete. Follow live events after the initial backward headCursor. Windows may split runs/tool batches; keep historical reconstruction separate from live controls.

- HTTP: `GET /api/v1/universes/{universe}/sessions/{sessionId}/events`
- Success: `200`
- Transfer: `json`
- Query: `after`, `before`, `direction`, `limit`, `waitMs`
- Path input fields: `sessionId` → `sessionId`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `session`
- Role: `viewer`

### `session/context/append`

**Append keyed session context**

Admits a batch of keyed context entries with per-entry results. The same content under a key is a no-op. Different content replaces the earlier entry, which leaves active context, and goes last under a new id; a catalog supersedes instead. Invalid input can fail one entry without discarding successful entries.

- HTTP: `POST /api/v1/universes/{universe}/sessions/{sessionId}/context/append`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `sessionId` → `sessionId`
- Request body: JSON object: `entries`
- Access: `{"action":"controlSession","kind":"universe"}`
- Group: `session`
- Role: `contributor`

### `session/context/remove`

**Remove keyed session context**

Removes active entries by stable key with per-key results. Missing keys are idempotent no-ops; runtime-reserved run keys cannot be removed.

- HTTP: `POST /api/v1/universes/{universe}/sessions/{sessionId}/context/remove`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `sessionId` → `sessionId`
- Request body: JSON object: `keys`
- Access: `{"action":"controlSession","kind":"universe"}`
- Group: `session`
- Role: `contributor`

### `session/context/replace`

**Replace session context entries**

Replaces active tool results and user messages in place by entry id, with per-entry results, e.g. to withdraw content the provider rejects. Entries keep their ids, positions, and kinds, so a tool result takes only text and its call stays answered. Refused while a run is active; ids no longer in context report absent.

- HTTP: `POST /api/v1/universes/{universe}/sessions/{sessionId}/context/replace`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `sessionId` → `sessionId`
- Request body: JSON object: `entries`
- Access: `{"action":"controlSession","kind":"universe"}`
- Group: `session`
- Role: `contributor`

### `session/context/compact`

**Compact session context**

Performs one standalone compaction in any automatic mode, including Disabled, and waits for its outcome. Active work queues it until a safe turn boundary. A failed compaction leaves the context unchanged and fails the call with its reason. A wait that runs out fails with operationPending while it can still complete; session events report the outcome.

- HTTP: `POST /api/v1/universes/{universe}/sessions/{sessionId}/context/compact`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `sessionId` → `sessionId`
- Request body: empty JSON object or no body
- Access: `{"action":"controlSession","kind":"universe"}`
- Group: `session`
- Role: `contributor`

### `session/runs/start`

**Start an agent run**

Returns once the run is accepted — queued behind an active run, or running — not when it finishes. A compacting session holds the request; if the wait runs out the call fails with operationPending and the submission id, and the run still starts later. Retrying with that id cannot start a second run. Follow session events or reread the session.

- HTTP: `POST /api/v1/universes/{universe}/sessions/{sessionId}/runs`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `sessionId` → `sessionId`
- Request body: JSON object: `config`, `notifyOnTerminal`, `source`, `submissionId`
- Access: `{"action":"controlSession","kind":"universe"}`
- Group: `session`
- Role: `contributor`

### `session/runs/list`

**List session runs**

Returns a newest-first keyset page of bounded run summaries, from current state for recent runs and from the session's log for older ones.

- HTTP: `GET /api/v1/universes/{universe}/sessions/{sessionId}/runs`
- Success: `200`
- Transfer: `json`
- Query: `cursor`, `limit`
- Path input fields: `sessionId` → `sessionId`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `session`
- Role: `viewer`

### `session/runs/read`

**Read one session run**

Projects one run from its events. A run past the server's detail limit keeps its opening and its most recent turns and sets earlierEventsOmitted; session/events/read has the rest.

- HTTP: `GET /api/v1/universes/{universe}/sessions/{sessionId}/runs/{runId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `sessionId` → `sessionId`, `runId` → `runId`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `session`
- Role: `viewer`

### `session/runs/cancel`

**Cancel a run**

Requests cancellation of the named queued or active run and returns its summary; observe session events for terminal completion. In-flight model and tool activity is aborted; no grace turn runs.

- HTTP: `POST /api/v1/universes/{universe}/sessions/{sessionId}/runs/{runId}/cancel`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `sessionId` → `sessionId`, `runId` → `runId`
- Request body: empty JSON object or no body
- Access: `{"action":"stopSession","kind":"universe"}`
- Group: `session`
- Role: `contributor`

### `session/runs/approvals/decide`

**Decide pending run approvals**

Approves or rejects pending MCP tool calls on the named active run. Valid decisions apply independently; the run resumes only after every pending approval has a decision. A decision whose wait runs out reports status pending and can still apply.

- HTTP: `POST /api/v1/universes/{universe}/sessions/{sessionId}/runs/{runId}/approvals/decide`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `sessionId` → `sessionId`, `runId` → `runId`
- Request body: JSON object: `decisions`
- Access: `{"action":"controlSession","kind":"universe"}`
- Group: `session`
- Role: `contributor`

### `session/runs/steer`

**Steer the active run**

Injects input into the named active run; the model sees it at the next turn boundary without interrupting the in-flight turn. A parked run holds it without waking. Rejected for queued, cancelling, or finished runs. Supply submissionId so a retry lands once; a wait that runs out fails with operationPending and the steering can still land.

- HTTP: `POST /api/v1/universes/{universe}/sessions/{sessionId}/runs/{runId}/steer`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `sessionId` → `sessionId`, `runId` → `runId`
- Request body: JSON object: `items`, `submissionId`
- Access: `{"action":"controlSession","kind":"universe"}`
- Group: `session`
- Role: `contributor`

### `session/skills/list`

**List available session skills**

Returns separate VFS and environment catalogs with source, reference, availability, readable skill paths, and warnings. Refreshes only when open with no active or queued run, without waking environments. Absent catalogs are omitted.

- HTTP: `GET /api/v1/universes/{universe}/sessions/{sessionId}/skills`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `sessionId` → `sessionId`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `session`
- Role: `viewer`

### `session/profiles/apply`

**Apply a profile to a session**

Applies a named or inline profile's config, instructions, and environment setup to an existing session; mutating profile sections require it to be open and idle. Pass current revisions to guard concurrent changes.

- HTTP: `POST /api/v1/universes/{universe}/sessions/{sessionId}/profiles/apply`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `sessionId` → `sessionId`
- Request body: JSON object: `expectedConfigRevision`, `expectedToolsRevision`, `profile`
- Access: `{"action":"configureSession","kind":"universe"}`
- Group: `session`
- Role: `operator`

### `session/environments/activate`

**Activate a session environment**

Selects an attached, live universe environment for environment-targeted tools while the session is idle.

- HTTP: `PUT /api/v1/universes/{universe}/sessions/{sessionId}/active-environment`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `sessionId` → `sessionId`
- Request body: JSON object: `environmentId`
- Access: `{"action":"configureSession","kind":"universe"}`
- Group: `session`
- Role: `operator`

### `session/environments/deactivate`

**Deactivate the session environment**

Clears active environment selection without changing or closing the universe environment.

- HTTP: `DELETE /api/v1/universes/{universe}/sessions/{sessionId}/active-environment`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `sessionId` → `sessionId`
- Request body: none
- Access: `{"action":"configureSession","kind":"universe"}`
- Group: `session`
- Role: `operator`

### `environments/credentials/bind`

**Bind a credential into an environment**

Maps an environment variable name to a retrievable credential grant or to a model API key provider's stored key. Commands in the environment receive the plain value, so a brokered grant and any other provider credential are refused. Requires configuring resources in the universe. The response exposes only the source handle, never secret material.

- HTTP: `PUT /api/v1/universes/{universe}/environments/{environmentId}/credentials/{envName}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `environmentId` → `environmentId`, `envName` → `envName`
- Request body: JSON object: `source`
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `environments`
- Role: `operator`

### `environments/credentials/list`

**List environment credential bindings**

Returns variable names and credential source handles for a universe environment; resolved secret values are never returned.

- HTTP: `GET /api/v1/universes/{universe}/environments/{environmentId}/credentials`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `environmentId` → `environmentId`
- Request body: none
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `environments`
- Role: `operator`

### `environments/credentials/unbind`

**Unbind an environment credential**

Removes one variable-to-credential mapping without deleting the underlying grant or provider credential.

- HTTP: `DELETE /api/v1/universes/{universe}/environments/{environmentId}/credentials/{envName}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `environmentId` → `environmentId`, `envName` → `envName`
- Request body: none
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `environments`
- Role: `operator`

### `environments/create`

**Create an environment**

Creates a provisioned environment, attributed to the caller, from a template of a provider enabled for the universe. Idempotent per requestId: same inputs, same environment; others conflict. Provisioning until the target's first envd handshake, then open; idle overrides the template default.

- HTTP: `POST /api/v1/universes/{universe}/environments`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: JSON object: `displayName`, `environmentId`, `idle`, `metadata`, `providerId`, `requestId`, `templateId`
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `environments`
- Role: `operator`

### `environments/read`

**Read an environment**

Returns one environment: its source, lifecycle state, derived display status, state message, close reason, daemon build, and the latest provider observation.

- HTTP: `GET /api/v1/universes/{universe}/environments/{environmentId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `environmentId` → `environmentId`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `environments`
- Role: `viewer`

### `environments/update`

**Update an environment**

Replaces the display name and caller metadata of an environment of any kind. Refused for a closed environment.

- HTTP: `PATCH /api/v1/universes/{universe}/environments/{environmentId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `environmentId` → `environmentId`
- Request body: JSON object: `displayName`, `metadata`
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `environments`
- Role: `operator`

### `environments/list`

**List environments**

Lists the universe environments in id order, one page at a time, optionally filtered by provider, registration key, state, or metadata. Closed environments are omitted unless includeClosed is set or state is closed. Continue from nextAfter.

- HTTP: `GET /api/v1/universes/{universe}/environments`
- Success: `200`
- Transfer: `json`
- Query: `after`, `includeClosed`, `limit`, `metadata`, `providerId`, `registrationKeyId`, `state`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `environments`
- Role: `viewer`

### `environments/close`

**Close an environment**

Closes an environment; idempotent. A provisioned environment moves to closing while Lightspeed asks its provider to remove the target, retrying until it is gone, then closed. External and registered environments close at once, and a connected registered daemon is told its environment closed.

- HTTP: `POST /api/v1/universes/{universe}/environments/{environmentId}/close`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `environmentId` → `environmentId`
- Request body: empty JSON object or no body
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `environments`
- Role: `operator`

### `environments/external/create`

**Register an external environment**

Creates an open environment backed by a Lightspeed-reachable envd endpoint, attributed to the caller. Idempotent per requestId. Reachability is checked on use.

- HTTP: `POST /api/v1/universes/{universe}/external-environments`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: JSON object: `displayName`, `endpoint`, `environmentId`, `metadata`, `requestId`
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `environments`
- Role: `operator`

### `environments/registered/put`

**Declare a registered environment**

Declares a fixed machine as an open environment with a chosen id before its daemon registers. The first daemon its persistent key admits under the given name is bound to it; until then it is away and tools wait. Repeating it changes nothing; display name and metadata converge. The key and name are fixed. Conflict: an id or name in use.

- HTTP: `PUT /api/v1/universes/{universe}/environments/{environmentId}/registration`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `environmentId` → `environmentId`
- Request body: JSON object: `displayName`, `metadata`, `name`, `registrationKeyId`
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `environments`
- Role: `operator`

### `environments/registered/release`

**Release a declared environment's daemon**

Forgets the daemon bound to a declared environment and ends its connection, so another daemon identity may bind under the same key and name. The environment keeps its id, its name, its credential bindings and every reference to it. No effect when no daemon is bound. Refused for an environment that was not declared.

- HTTP: `POST /api/v1/universes/{universe}/environments/{environmentId}/release`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `environmentId` → `environmentId`
- Request body: empty JSON object or no body
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `environments`
- Role: `operator`

### `environments/ingress/put`

**Configure environment public ingress**

Synchronously ensures or removes public ingress of an open provisioned environment through its provider; source.publicEndpoint carries the reported endpoint. Rejected when the provider offers no ingress; an internal error, safe to retry, when the provider is unreachable or times out. The provider owns hostname, guest port, routing, TLS, and health.

- HTTP: `PUT /api/v1/universes/{universe}/environments/{environmentId}/ingress`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `environmentId` → `environmentId`
- Request body: JSON object: `enabled`
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `environments`
- Role: `operator`

### `environments/power/put`

**Set environment power**

Synchronously moves an open provisioned environment's target to running, paused, suspended, or stopped via its provider and records the observation; the provider may also change power on idle. Rejected without provider power control or when the template lacks the state; an internal error, safe to retry, when the provider is unreachable or times out.

- HTTP: `PUT /api/v1/universes/{universe}/environments/{environmentId}/power`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `environmentId` → `environmentId`
- Request body: JSON object: `power`
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `environments`
- Role: `operator`

### `environments/providers/list`

**List available environment providers**

Lists the deployment providers this universe may create environments from: those available to every universe, and those this universe is bound to.

- HTTP: `GET /api/v1/universes/{universe}/environment-providers`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `environments`
- Role: `viewer`

### `environments/providers/read`

**Read an available environment provider**

Returns one provider this universe may create environments from; not found when it may not. Templates and physical resource, network, and ingress policy remain provider-owned.

- HTTP: `GET /api/v1/universes/{universe}/environment-providers/{providerId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `providerId` → `providerId`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `environments`
- Role: `viewer`

### `environments/templates/list`

**List environment templates**

Reads templates live from the controllers of every provider available to the universe, or of one provider: compute, power states, public ingress, and default idle policy.

- HTTP: `GET /api/v1/universes/{universe}/environment-providers/{providerId}/templates`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `providerId` → `providerId`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `environments`
- Role: `viewer`

### `environments/templates/read`

**Read an environment template**

Returns one template, read live from the controller of a provider available to the universe.

- HTTP: `GET /api/v1/universes/{universe}/environment-providers/{providerId}/templates/{templateId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `providerId` → `providerId`, `templateId` → `templateId`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `environments`
- Role: `viewer`

### `environments/jobs/create`

**Create environment jobs**

Starts a dependency-aware job group on one environment instance, injecting its configured credentials at provider start. requestId is the retry identity; jobs are owned by the instance, not a session. A powered-down environment wakes on its data route; the call fails with environmentNotReady while it wakes or reconnects; retry with backoff.

- HTTP: `POST /api/v1/universes/{universe}/environments/{environmentId}/jobs`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `environmentId` → `environmentId`
- Request body: JSON object: `jobs`, `requestId`
- Access: `{"action":"useResource","kind":"universe"}`
- Group: `environments`
- Role: `contributor`

### `environments/jobs/read`

**Read environment jobs**

Reads selected job handles with bounded output from a byte offset or the output tail; continue from each entry's outputNext and poll on the returned status.

- HTTP: `GET /api/v1/universes/{universe}/environment-jobs`
- Success: `200`
- Transfer: `json`
- Query: `after`, `jobs`, `maxBytes`, `tailBytes`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `environments`
- Role: `viewer`

### `environments/jobs/cancel`

**Cancel environment jobs**

Requests cancellation for selected jobs, optionally including dependents. Force is provider-specific escalation; inspect each per-job result.

- HTTP: `POST /api/v1/universes/{universe}/environment-jobs/cancel`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: JSON object: `force`, `jobs`, `scope`
- Access: `{"action":"useResource","kind":"universe"}`
- Group: `environments`
- Role: `contributor`

### `environments/registration-keys/create`

**Mint an environment registration key**

Creates a reusable universe-scoped key that lets outbound envd daemons register as environments. The plaintext secret is returned exactly once; only its hash is stored. Identity mode (immutable), active limit, disconnect grace, offline close-after, and expiry are the key's policy. Treat the secret like a cluster-join credential.

- HTTP: `POST /api/v1/universes/{universe}/environment-registration-keys`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: JSON object: `declaredOnly`, `displayName`, `ephemeralDisconnectGraceMs`, `expiresAtMs`, `identityMode`, `maxActiveEnvironments`, `offlineCloseAfterMs`, `registrationKeyId`
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `environments`
- Role: `operator`

### `environments/registration-keys/put`

**Declare an environment registration key**

Makes the key of this id exist with this policy and answer to exactly the supplied secret. Repeating it changes nothing. A missing key is created; a different secret retires the previous one at once, as rotation does. Conflict: a changed identity mode, a revoked key, or a secret that is or was a key's.

- HTTP: `PUT /api/v1/universes/{universe}/environment-registration-keys/{registrationKeyId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `registrationKeyId` → `registrationKeyId`
- Request body: JSON object: `declaredOnly`, `displayName`, `ephemeralDisconnectGraceMs`, `expiresAtMs`, `identityMode`, `maxActiveEnvironments`, `offlineCloseAfterMs`, `secret`
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `environments`
- Role: `operator`

### `environments/registration-keys/read`

**Read an environment registration key**

Returns the key's display prefix, policy, status, and derived environment counts; never the secret or its hash.

- HTTP: `GET /api/v1/universes/{universe}/environment-registration-keys/{registrationKeyId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `registrationKeyId` → `registrationKeyId`
- Request body: none
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `environments`
- Role: `operator`

### `environments/registration-keys/list`

**List environment registration keys**

Lists this universe's registration keys with policy, status, and derived counts. Each key is the group of the environments it admitted.

- HTTP: `GET /api/v1/universes/{universe}/environment-registration-keys`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: none
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `environments`
- Role: `operator`

### `environments/registration-keys/update`

**Update an environment registration key**

Replaces a key's editable policy: display name, active limit, disconnect grace, offline close-after, and expiry; an omitted field is unset. The identity mode is immutable. The active limit and expiry apply to later admissions; a changed disconnect grace or offline close-after applies at the next sweep to environments whose daemon is away.

- HTTP: `PATCH /api/v1/universes/{universe}/environment-registration-keys/{registrationKeyId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `registrationKeyId` → `registrationKeyId`
- Request body: JSON object: `declaredOnly`, `displayName`, `ephemeralDisconnectGraceMs`, `expiresAtMs`, `maxActiveEnvironments`, `offlineCloseAfterMs`
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `environments`
- Role: `operator`

### `environments/registration-keys/rotate`

**Rotate an environment registration key**

Replaces the key's secret and display prefix and retires the old secret at once. The key keeps its id, group, policy, and the environments it admitted; daemons already registered are unaffected because they never present the key again. Returns the new secret once. Refused for a revoked key.

- HTTP: `POST /api/v1/universes/{universe}/environment-registration-keys/{registrationKeyId}/rotate`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `registrationKeyId` → `registrationKeyId`
- Request body: empty JSON object or no body
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `environments`
- Role: `operator`

### `environments/registration-keys/revoke`

**Revoke an environment registration key**

Stops the key from admitting new daemon identities; already registered daemons keep reconnecting. With closeEnvironments, also closes every non-closed environment the key admitted. Idempotent.

- HTTP: `POST /api/v1/universes/{universe}/environment-registration-keys/{registrationKeyId}/revoke`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `registrationKeyId` → `registrationKeyId`
- Request body: JSON object: `closeEnvironments`
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `environments`
- Role: `operator`

### `transcriptions/start`

**Start transcription**

Admit or rejoin a requester-scoped audio transcription. The resolved model is immutable. No session or run is created.

- HTTP: `POST /api/v1/universes/{universe}/transcriptions`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: JSON object: `audio`, `idempotencyKey`, `language`, `model`, `prompt`
- Access: `{"action":"useResource","kind":"universe"}`
- Group: `transcriptions`
- Role: `contributor`

### `transcriptions/read`

**Read transcription**

Read status and transcript text. Caller visibility is enforced by the external gate. Unsubmitted CAS results may expire.

- HTTP: `GET /api/v1/universes/{universe}/transcriptions/{transcriptionId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `transcriptionId` → `transcriptionId`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `transcriptions`
- Role: `viewer`

### `transcriptions/cancel`

**Cancel transcription**

Cancel unfinished transcription. Repeated cancellation is safe; completed results remain unchanged. Caller ownership is enforced by the external gate.

- HTTP: `POST /api/v1/universes/{universe}/transcriptions/{transcriptionId}/cancel`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `transcriptionId` → `transcriptionId`
- Request body: empty JSON object or no body
- Access: `{"action":"useResource","kind":"universe"}`
- Group: `transcriptions`
- Role: `contributor`

### `models/defaults/read`

**Read universe model defaults**

Returns the revision and independent agentRun and speechToText selections. Revision zero means no update has been made. Does not contact model providers.

- HTTP: `GET /api/v1/universes/{universe}/model-defaults`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `models`
- Role: `viewer`

### `models/defaults/put`

**Set a universe model default**

Sets or explicitly clears one purpose slot. With expectedRevision the write is refused if the defaults changed since. Existing sessions and admitted work keep their model. Validates the purpose and protocol without contacting provider discovery.

- HTTP: `PUT /api/v1/universes/{universe}/model-defaults/{slot}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `slot` → `slot`
- Request body: JSON object: `expectedRevision`, `model`
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `models`
- Role: `operator`

### `models/list`

**Discover available models**

Queries supported providers directly, with a brief process-local burst cache, and returns best-effort selectable routes. One provider failure does not discard successful results from others.

- HTTP: `GET /api/v1/universes/{universe}/models`
- Success: `200`
- Transfer: `json`
- Query: `selectableOnly`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `models`
- Role: `viewer`

### `profiles/create`

**Create an agent profile**

Creates a new universe-scoped reusable profile document; use profiles/put for create-or-replace revision semantics.

- HTTP: `POST /api/v1/universes/{universe}/profiles`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: JSON object: `profile`
- Access: `{"action":"createProfile","kind":"universe"}`
- Group: `profiles`
- Role: `operator`

### `profiles/read`

**Read an agent profile**

Returns the complete profile document and current revision.

- HTTP: `GET /api/v1/universes/{universe}/profiles/{profileId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `profileId` → `profileId`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `profiles`
- Role: `viewer`

### `profiles/list`

**List agent profiles**

Returns lightweight summaries of universe-scoped reusable profiles.

- HTTP: `GET /api/v1/universes/{universe}/profiles`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `profiles`
- Role: `viewer`

### `profiles/put`

**Create or replace an agent profile**

Stores the complete profile document. Use expectedRevision from profiles/read when replacing to prevent lost updates; absence writes unconditionally.

- HTTP: `PUT /api/v1/universes/{universe}/profiles/{profileId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `profileId` → `profile.profileId`
- Request body: JSON object: `expectedRevision`, `profile`
- Access: `{"action":"manageProfile","kind":"universe"}`
- Group: `profiles`
- Role: `operator`

### `profiles/delete`

**Delete an agent profile**

Deletes the catalog document; sessions previously created or configured from it retain their materialized state.

- HTTP: `DELETE /api/v1/universes/{universe}/profiles/{profileId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `profileId` → `profileId`
- Request body: none
- Access: `{"action":"manageProfile","kind":"universe"}`
- Group: `profiles`
- Role: `operator`

### `blobs/put`

**Store content-addressed blobs**

Decodes and stores a batch of base64 payloads, returning immutable content references in request order. Re-uploading identical bytes is naturally deduplicated.

- HTTP: `POST /api/v1/universes/{universe}/blobs/batch`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: JSON object: `blobs`
- Access: `{"action":"useResource","kind":"universe"}`
- Group: `blobs/put`
- Role: `contributor`

### `blobs/compose`

**Join uploaded parts into one blob**

Streams the part blobs, in order, into blobRef and verifies its length and SHA-256 before publishing. Upload a large file as parts with blobs/upload, use blobs/has to resume, then compose. Composing existing content only renews its grace. Parts stay ordinary blobs, collected after the usual grace period.

- HTTP: `POST /api/v1/universes/{universe}/blobs/{blobRef}/compose`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `blobRef` → `blobRef`
- Request body: JSON object: `parts`, `sizeBytes`
- Access: `{"action":"useResource","kind":"universe"}`
- Group: `blobs/put`
- Role: `contributor`

### `blobs/read`

**Read a content-addressed blob**

Streams immutable blob bytes from this universe. Accepts one `Range: bytes=` range (start-, start-end or -suffix) and answers it with 206 and Content-Range; other ranges are ignored and the whole blob is sent. An unsatisfiable range gets 416 with the blob length. Content-Length reports the size of the sent bytes.

- HTTP: `GET /api/v1/universes/{universe}/blobs/{blobRef}/content`
- Success: `200`
- Transfer: `binaryDownload`
- Query: none
- Path input fields: `blobRef` → `blobRef`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `session`
- Role: `viewer`

### `blobs/has`

**Check blob availability**

Checks a batch of content references without returning blob bodies, preserving request order.

- HTTP: `GET /api/v1/universes/{universe}/blobs/presence`
- Success: `200`
- Transfer: `json`
- Query: `blobRefs`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `session`
- Role: `viewer`

### `vfs/snapshots/commit`

**Commit a VFS snapshot**

Validates and stores an immutable filesystem manifest. Upload referenced file blobs first; the returned snapshot ref is content-addressed.

- HTTP: `POST /api/v1/universes/{universe}/vfs/snapshots`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: JSON object: `manifest`
- Access: `{"action":"useResource","kind":"universe"}`
- Group: `vfs`
- Role: `contributor`

### `vfs/snapshots/read`

**Read a VFS snapshot**

Returns an immutable snapshot manifest and aggregate file/byte counts; file bodies remain separate blobs.

- HTTP: `GET /api/v1/universes/{universe}/vfs/snapshots/{snapshotRef}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `snapshotRef` → `snapshotRef`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `vfs`
- Role: `viewer`

### `vfs/workspaces/create`

**Create a mutable VFS workspace**

Creates a universe workspace attributed to the caller at an optional seed snapshot; absence starts from a server-created empty snapshot.

- HTTP: `POST /api/v1/universes/{universe}/vfs/workspaces`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: JSON object: `displayName`, `snapshotRef`, `workspaceId`
- Access: `{"action":"createWorkspace","kind":"universe"}`
- Group: `vfs`
- Role: `operator`

### `vfs/workspaces/read`

**Read a VFS workspace**

Returns workspace metadata, current head snapshot, and revision for safe updates.

- HTTP: `GET /api/v1/universes/{universe}/vfs/workspaces/{workspaceId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `workspaceId` → `workspaceId`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `vfs`
- Role: `viewer`

### `vfs/workspaces/list`

**List VFS workspaces**

Lists the mutable universe workspaces with head snapshots, sizes, and revisions.

- HTTP: `GET /api/v1/universes/{universe}/vfs/workspaces`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `vfs`
- Role: `viewer`

### `vfs/workspaces/update`

**Update a VFS workspace**

Moves the workspace head to an existing snapshot, which requires use of the workspace, and updates its display name, which requires configuring it. Pass expectedRevision from a read to prevent lost updates.

- HTTP: `PATCH /api/v1/universes/{universe}/vfs/workspaces/{workspaceId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `workspaceId` → `workspaceId`
- Request body: JSON object: `displayName`, `expectedRevision`, `snapshotRef`
- Access: `{"action":"useResource","kind":"universe"}`
- Group: `vfs`
- Role: `contributor`

### `vfs/workspaces/delete`

**Delete a VFS workspace**

Deletes the mutable workspace record; immutable snapshots and blobs remain content-addressed resources.

- HTTP: `DELETE /api/v1/universes/{universe}/vfs/workspaces/{workspaceId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `workspaceId` → `workspaceId`
- Request body: none
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `vfs`
- Role: `operator`

### `mcp/servers/put`

**Create or replace an MCP server record**

Stores the complete catalog document with its optional credential grant. A new server is attributed to the caller. Use expectedRevision when replacing; token material is never accepted or returned, but serverUrl is stored and shown as written, so keep secrets out of it.

- HTTP: `PUT /api/v1/universes/{universe}/mcp/servers/{serverId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `serverId` → `server.serverId`
- Request body: JSON object: `expectedRevision`, `server`
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `mcp`
- Role: `operator`

### `mcp/servers/auth/discover`

**Discover MCP server authentication**

Looks for standards-based OAuth protected-resource metadata without creating a server, OAuth client, flow, or grant. An absent OAuth result is inconclusive and callers must allow manual auth selection.

- HTTP: `POST /api/v1/universes/{universe}/mcp/auth/discover`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: JSON object: `serverUrl`
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `mcp`
- Role: `operator`

### `mcp/servers/tools/discover`

**Discover MCP server tools**

Connects directly to the configured MCP server with its current universe credential and returns one bounded live tools/list result. The inventory is never persisted or cached and no tool is invoked.

- HTTP: `POST /api/v1/universes/{universe}/mcp/servers/{serverId}/tools/discover`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `serverId` → `serverId`
- Request body: empty JSON object or no body
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `mcp`
- Role: `operator`

### `mcp/servers/read`

**Read an MCP server record**

Returns one catalog document with defaults, auth policy, non-secret grant binding, status, and revision; no credential value is exposed.

- HTTP: `GET /api/v1/universes/{universe}/mcp/servers/{serverId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `serverId` → `serverId`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `mcp`
- Role: `viewer`

### `mcp/servers/list`

**List MCP server records**

Lists the universe catalog entries, optionally filtered by lifecycle/configuration status.

- HTTP: `GET /api/v1/universes/{universe}/mcp/servers`
- Success: `200`
- Transfer: `json`
- Query: `status`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `mcp`
- Role: `viewer`

### `mcp/servers/delete`

**Delete an MCP server record**

Deletes the catalog document. Existing session configs that reference it are not silently rewritten and may need explicit reconfiguration.

- HTTP: `DELETE /api/v1/universes/{universe}/mcp/servers/{serverId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `serverId` → `serverId`
- Request body: none
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `mcp`
- Role: `operator`

### `credentials/grants/import`

**Import a token credential**

Accepts a plaintext token, encrypts it immediately, and returns only grant metadata/token-presence flags. Brokered is the default; exposure is immutable, and only a retrievable credential can be leased to a service or bound into an environment.

- HTTP: `POST /api/v1/universes/{universe}/credentials/grants/actions/import`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: JSON object: `audience`, `displayName`, `expiresAtMs`, `exposure`, `grantId`, `metadata`, `providerId`, `scopes`, `subjectHint`, `token`
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `credentials`
- Role: `operator`

### `credentials/grants/read`

**Read credential grant metadata**

Returns creator attribution, provider binding, scopes, audience, expiry, status, and token-presence flags; access and refresh token values are never returned.

- HTTP: `GET /api/v1/universes/{universe}/credentials/grants/{grantId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `grantId` → `grantId`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `credentials`
- Role: `viewer`

### `credentials/grants/list`

**List credential grants**

Lists non-secret grant metadata for the universe, optionally filtered by status.

- HTTP: `GET /api/v1/universes/{universe}/credentials/grants`
- Success: `200`
- Transfer: `json`
- Query: `status`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `credentials`
- Role: `viewer`

### `credentials/grants/put`

**Declare a token credential**

Makes the token credential of this id exist with these settings and hold exactly the supplied token, encrypted on receipt. Repeating it changes nothing. A missing credential is created; a different token replaces the stored one for everything referencing the id. Conflict: another kind of grant, a revoked one, or changed exposure.

- HTTP: `PUT /api/v1/universes/{universe}/credentials/grants/{grantId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `grantId` → `grantId`
- Request body: JSON object: `audience`, `displayName`, `dryRun`, `expiresAtMs`, `exposure`, `metadata`, `providerId`, `scopes`, `subjectHint`, `token`
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `credentials`
- Role: `operator`

### `credentials/grants/revoke`

**Revoke a credential grant**

Marks the grant unusable by token consumers while retaining non-secret audit metadata.

- HTTP: `POST /api/v1/universes/{universe}/credentials/grants/{grantId}/revoke`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `grantId` → `grantId`
- Request body: empty JSON object or no body
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `credentials`
- Role: `operator`

### `credentials/clients/create`

**Register an OAuth client**

Stores provider endpoints and client identity; an optional plaintext client secret is encrypted and represented thereafter only by hasClientSecret.

- HTTP: `POST /api/v1/universes/{universe}/credentials/clients`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: JSON object: `audience`, `authorizationEndpoint`, `authorizationResponseIssParameterSupported`, `authorizationServerIssuer`, `authorizationServerScopesSupported`, `clientId`, `clientSecret`, `displayName`, `providerId`, `providerKind`, `remoteClientId`, `scopesDefault`, `tokenEndpoint`, `tokenEndpointAuthMethod`
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `credentials`
- Role: `operator`

### `credentials/clients/read`

**Read OAuth client metadata**

Returns endpoints, public client identity, defaults, and secret-presence state; the client secret is never returned.

- HTTP: `GET /api/v1/universes/{universe}/credentials/clients/{clientId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `clientId` → `clientId`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `credentials`
- Role: `viewer`

### `credentials/clients/list`

**List OAuth clients**

Lists non-secret OAuth client registrations in the universe.

- HTTP: `GET /api/v1/universes/{universe}/credentials/clients`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `credentials`
- Role: `viewer`

### `credentials/clients/delete`

**Delete an OAuth client**

Deletes the client registration and its stored client secret; grants already created from it remain separate records.

- HTTP: `DELETE /api/v1/universes/{universe}/credentials/clients/{clientId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `clientId` → `clientId`
- Request body: none
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `credentials`
- Role: `operator`

### `credentials/flows/start`

**Start an OAuth authorization flow**

Creates a short-lived PKCE flow carrying the immutable grant exposure choice and returns a browser authorization URL containing one-time state. Treat the URL as sensitive and poll credentials/flows/read for completion.

- HTTP: `POST /api/v1/universes/{universe}/credentials/flows`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: JSON object: `audience`, `clientId`, `exposure`, `scopes`
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `credentials`
- Role: `operator`

### `credentials/flows/read`

**Read OAuth flow status**

Polls a flow's pending/completed/failed/expired state and returns the resulting grant id when authorization succeeds; no token value is exposed.

- HTTP: `GET /api/v1/universes/{universe}/credentials/flows/{flowId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `flowId` → `flowId`
- Request body: none
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `credentials`
- Role: `operator`

### `credentials/providers/create`

**Register a credential provider**

Creates a model or GitHub credential source. Plaintext API keys/private keys are encrypted on receipt and later represented only by configuration plus hasCredential.

- HTTP: `POST /api/v1/universes/{universe}/credentials/providers`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: JSON object: `config`, `credential`, `displayName`, `providerId`
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `credentials`
- Role: `operator`

### `credentials/providers/put`

**Declare a credential provider**

Makes the provider of this id exist with this configuration and credential. Repeating it changes nothing. A missing provider is created; one that differs is rewritten in place after the new settings validate, atomically, and its old credential deleted. Sessions and environment variables referencing the id keep working.

- HTTP: `PUT /api/v1/universes/{universe}/credentials/providers/{providerId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `providerId` → `providerId`
- Request body: JSON object: `config`, `credential`, `displayName`, `dryRun`
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `credentials`
- Role: `operator`

### `credentials/providers/read`

**Read credential provider metadata**

Returns provider kind, non-secret configuration, credential-presence state, and status; stored credentials are never returned.

- HTTP: `GET /api/v1/universes/{universe}/credentials/providers/{providerId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `providerId` → `providerId`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `credentials`
- Role: `viewer`

### `credentials/providers/list`

**List credential providers**

Lists non-secret model/GitHub provider registrations for the universe.

- HTTP: `GET /api/v1/universes/{universe}/credentials/providers`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `credentials`
- Role: `viewer`

### `credentials/providers/delete`

**Delete a credential provider**

Deletes the provider registration and its directly stored credential; separately stored grants remain independent records.

- HTTP: `DELETE /api/v1/universes/{universe}/credentials/providers/{providerId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `providerId` → `providerId`
- Request body: none
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `credentials`
- Role: `operator`

### `credentials/github/installations/list`

**List GitHub App installations**

Uses the registered GitHub App provider credential to query accessible installations and returns account/permission metadata without tokens.

- HTTP: `GET /api/v1/universes/{universe}/credentials/providers/{providerId}/github/installations`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `providerId` → `providerId`
- Request body: none
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `credentials`
- Role: `operator`

### `credentials/github/installations/grant`

**Grant access to a GitHub App installation**

Creates or refreshes a universe credential grant for one accessible installation. This method never returns a token: the broker mints short-lived installation tokens when the grant is used. Brokered is the default; only a grant created retrievable can be bound into an environment or leased to a service.

- HTTP: `POST /api/v1/universes/{universe}/credentials/providers/{providerId}/github/installations/{installationId}/grant`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `providerId` → `providerId`, `installationId` → `installationId`
- Request body: JSON object: `displayName`, `exposure`, `grantId`
- Access: `{"action":"configureResource","kind":"universe"}`
- Group: `credentials`
- Role: `operator`

### `bots/create`

**Create a bot**

Creates the bot record, optionally with its triggers, and starts its controller. Fails if the bot id exists; a trigger failure rolls the bot back. Returns the created webhook triggers' ingest paths by trigger id.

- HTTP: `POST /api/v1/universes/{universe}/bots`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: JSON object: `bot`, `triggers`
- Access: `{"action":"createBot","kind":"universe"}`
- Group: `bots`
- Role: `operator`

### `bots/put`

**Create or replace a bot document**

Replaces the mutable configuration whole and signals the controller, which applies it at its next idle boundary. Pass expectedRevision when replacing; a closed bot accepts label-only edits.

- HTTP: `PUT /api/v1/universes/{universe}/bots/{botId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `botId` → `bot.botId`
- Request body: JSON object: `bot`, `expectedRevision`
- Access: `{"action":"manageBot","kind":"universe"}`
- Group: `bots`
- Role: `operator`

### `bots/read`

**Read a bot**

Returns the bot record, its current revision, and lifecycle columns.

- HTTP: `GET /api/v1/universes/{universe}/bots/{botId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `botId` → `botId`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `bots`
- Role: `viewer`

### `bots/list`

**List bots**

Returns the roster: every bot with its trigger count, pending event count, and latest event, optionally narrowed by createdBy.

- HTTP: `GET /api/v1/universes/{universe}/bots`
- Success: `200`
- Transfer: `json`
- Query: `createdBy`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `bots`
- Role: `viewer`

### `bots/close`

**Close a bot**

Terminal and idempotent: disables every trigger, drops schedules, and tells the controller to archive pending events and force-close its sessions. Returns once signalled; follow bots/state/read for closing to closed.

- HTTP: `POST /api/v1/universes/{universe}/bots/{botId}/close`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `botId` → `botId`
- Request body: empty JSON object or no body
- Access: `{"action":"manageBot","kind":"universe"}`
- Group: `bots`
- Role: `operator`

### `bots/delete`

**Delete a bot**

Closes the bot if needed, waits for its controller to complete, retains its session history, and removes the record so the bot id is free again.

- HTTP: `DELETE /api/v1/universes/{universe}/bots/{botId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `botId` → `botId`
- Request body: none
- Access: `{"action":"manageBot","kind":"universe"}`
- Group: `bots`
- Role: `operator`

### `bots/state/read`

**Read bot controller state**

Queries the controller workflow for its live snapshot (sessions, buffers, active and recent deliveries, budget) and lists sub-agent descendants. The controller is absent until the bot's first event.

- HTTP: `GET /api/v1/universes/{universe}/bots/{botId}/state`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `botId` → `botId`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `bots`
- Role: `viewer`

### `bots/sessions/rotate`

**Rotate a bot session**

Asks the controller to close one of the bot's sessions at its next idle boundary and continue on a fresh generation; queued deliveries follow.

- HTTP: `POST /api/v1/universes/{universe}/bots/{botId}/sessions/rotate`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `botId` → `botId`
- Request body: JSON object: `sessionId`
- Access: `{"action":"manageBot","kind":"universe"}`
- Group: `bots`
- Role: `operator`

### `bots/triggers/put`

**Create or replace a trigger**

Validates the trigger document (CEL parses, grants exist, one inbox per bot), reconciles its Temporal Schedule, and stores it. A poll spec edit resets the cursor; a webhook keeps its URL token. Returns a webhook's ingest path beside the view.

- HTTP: `PUT /api/v1/universes/{universe}/bots/{botId}/triggers/{triggerId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `botId` → `botId`, `triggerId` → `trigger.triggerId`
- Request body: JSON object: `expectedRevision`, `trigger`
- Access: `{"action":"manageBot","kind":"universe"}`
- Group: `bots`
- Role: `operator`

### `bots/triggers/read`

**Read a trigger**

Returns one trigger with its incidents and cursor. Read a webhook's ingest path through bots/triggers/secrets/read.

- HTTP: `GET /api/v1/universes/{universe}/bots/{botId}/triggers/{triggerId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `botId` → `botId`, `triggerId` → `triggerId`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `bots`
- Role: `viewer`

### `bots/triggers/list`

**List a bot's triggers**

Returns every trigger of the bot ordered by id, without ingest paths.

- HTTP: `GET /api/v1/universes/{universe}/bots/{botId}/triggers`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `botId` → `botId`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `bots`
- Role: `viewer`

### `bots/triggers/secrets/read`

**Read a trigger's secrets**

Returns the webhook ingest path. Its token lets the holder post events to the bot. Requires bot-management permission.

- HTTP: `GET /api/v1/universes/{universe}/bots/{botId}/triggers/{triggerId}/secrets`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `botId` → `botId`, `triggerId` → `triggerId`
- Request body: none
- Access: `{"action":"manageBot","kind":"universe"}`
- Group: `bots`
- Role: `operator`

### `bots/triggers/delete`

**Delete a trigger**

Drops the trigger's Temporal Schedule, then the record; stored events keep their history.

- HTTP: `DELETE /api/v1/universes/{universe}/bots/{botId}/triggers/{triggerId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `botId` → `botId`, `triggerId` → `triggerId`
- Request body: none
- Access: `{"action":"manageBot","kind":"universe"}`
- Group: `bots`
- Role: `operator`

### `bots/events/admit`

**Admit an event manually**

Stores an operator-authored event for the bot's main session and wakes the controller. eventId is the dedupe identity; a duplicate returns the stored row.

- HTTP: `POST /api/v1/universes/{universe}/bots/{botId}/events`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `botId` → `botId`
- Request body: JSON object: `event`
- Access: `{"action":"invokeBot","kind":"universe"}`
- Group: `bots`
- Role: `contributor`

### `bots/events/replay`

**Replay a stored event**

Re-admits the stored envelope as a fresh event with the original routing; the replay never coalesces.

- HTTP: `POST /api/v1/universes/{universe}/bots/{botId}/events/{seq}/replay`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `botId` → `botId`, `seq` → `seq`
- Request body: empty JSON object or no body
- Access: `{"action":"manageBot","kind":"universe"}`
- Group: `bots`
- Role: `operator`

### `bots/events/list`

**List a bot's events**

Cursor-paginated event log, newest first, with outcomes; payload documents stay in the CAS.

- HTTP: `GET /api/v1/universes/{universe}/bots/{botId}/events`
- Success: `200`
- Transfer: `json`
- Query: `cursor`, `limit`
- Path input fields: `botId` → `botId`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `bots`
- Role: `viewer`

### `bots/events/read`

**Read an event by number**

Returns the event row and its full stored envelope document.

- HTTP: `GET /api/v1/universes/{universe}/bots/{botId}/events/{seq}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `botId` → `botId`, `seq` → `seq`
- Request body: none
- Access: `{"action":"read","kind":"universe"}`
- Group: `bots`
- Role: `viewer`

### `bots/filters/test`

**Test a CEL filter**

Evaluates a filter against one payload or a sample of recent stored events, reporting matches and evaluation errors without changing anything.

- HTTP: `POST /api/v1/universes/{universe}/bots/{botId}/filters/test`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `botId` → `botId`
- Request body: JSON object: `filter`, `limit`, `payload`
- Access: `{"action":"manageBot","kind":"universe"}`
- Group: `bots`
- Role: `operator`

### `blobs/upload`

**Upload a content-addressed blob**

Streams bytes into immutable universe storage. The path is the expected content digest; Content-Length is required and both length and digest are verified. An identical upload is idempotent. Admission refreshes the blob's collection grace period.

- HTTP: `PUT /api/v1/universes/{universe}/blobs/{blobRef}/content`
- Success: `200`
- Transfer: `binaryUpload`
- Query: none
- Path input fields: `blobRef` → `blobRef`
- Request body: `application/octet-stream`; `Content-Length` required
- Access: `{"action":"useResource","kind":"universe"}`
- Group: `blobs/put`
- Role: `contributor`


## Service operations

### `credentials/grants/lease`

**Lease a retrievable credential grant**

Service callers only. Resolves the current access token through the broker, records the lease, and returns it once. Cache only in memory until expiry minus margin (or at most five minutes without expiry), re-lease after target 401/403, and never persist or place the token in workflow payloads.

- HTTP: `POST /api/v1/universes/{universe}/credentials/grants/{grantId}/lease`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `grantId` → `grantId`
- Request body: JSON object: `audience`
- Access: `{"kind":"service"}`
- Group: `credentials/lease`
- Role: `none`


## Deployment operations

### `deployment/universes/deleted/list`

**List deleted universes**

Permanent deletion records, newest first, with resource counts and unconfirmed provider targets.

- HTTP: `GET /api/v1/deleted-universes`
- Success: `200`
- Transfer: `json`
- Query: `after`
- Request body: none
- Access: `{"kind":"deployment"}`
- Group: `deployment/universes`
- Role: `none`

### `deployment/sessions/deleted/list`

**List deleted sessions**

Administrative metadata for soft-deleted sessions, ordered by session ID. Unavailable through universe-scoped methods.

- HTTP: `GET /api/v1/universes/{universe}/deleted-sessions`
- Success: `200`
- Transfer: `json`
- Query: `after`
- Path input fields: `universe` → `universe`
- Request body: none
- Access: `{"kind":"deployment"}`
- Group: `deployment/sessions`
- Role: `none`

### `deployment/sessions/purge`

**Permanently delete a session**

Removes a soft-deleted session and its soft-deleted history subtree. Rejects any selected session that has not been soft-deleted. Releases event, checkpoint and blob references; unreferenced blobs follow normal garbage collection. Does not delete external workspaces, environments, backups or Temporal history.

- HTTP: `DELETE /api/v1/universes/{universe}/deleted-sessions/{sessionId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `universe` → `universe`, `sessionId` → `sessionId`
- Request body: none
- Access: `{"kind":"deployment"}`
- Group: `deployment/sessions`
- Role: `none`

### `deployment/environment-provider-bindings/list`

**List a universe's deployment provider bindings**

Deployment configuration inventory of one universe's provider bindings.

- HTTP: `GET /api/v1/universes/{universe}/deployment-provider-bindings`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `universe` → `universe`
- Request body: none
- Access: `{"kind":"deployment"}`
- Group: `deployment/environment-providers`
- Role: `none`

### `deployment/universes/create`

**Create a universe**

Creates a universe for an explicit UUID with optional handle, metadata, protection and future expiry. Protection and expiry are mutually exclusive. Idempotent: existing universes retain all settings. A claimed handle or deleting/deleted UUID returns conflict.

- HTTP: `POST /api/v1/universes`
- Success: `200`, `201` (`created: true` selects 201; false selects 200)
- Transfer: `json`
- Query: none
- Request body: JSON object: `expiresAtMs`, `handle`, `metadata`, `protected`, `universeId`
- Access: `{"kind":"deployment"}`
- Group: `deployment/universes`
- Role: `none`

### `deployment/universes/list`

**List universes**

Returns deployment-wide summaries, including metadata, protection, expiry and deleting universes. The metadata filter requires every supplied key/value pair. Aggregate counts and last session activity are approximate.

- HTTP: `GET /api/v1/universes`
- Success: `200`
- Transfer: `json`
- Query: `metadata`
- Request body: none
- Access: `{"kind":"deployment"}`
- Group: `deployment/universes`
- Role: `none`

### `deployment/universes/read`

**Read a universe**

Returns one deployment tenant summary, addressed by UUID or active handle, with aggregate session, workspace, profile, and blob usage. Deleting universes remain readable by UUID. A permanently deleted UUID returns gone with its deletion record; an unknown UUID returns notFound.

- HTTP: `GET /api/v1/universes/{universe}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `universe` → `universe`
- Request body: none
- Access: `{"kind":"deployment"}`
- Group: `deployment/universes`
- Role: `none`

### `deployment/universes/configuration/put`

**Update universe configuration**

Updates supplied displayName, metadata and protected. Omitted fields retain their values; metadata replaces the complete map. Protection refuses deletion and cannot coexist with an expiry. Clearing protection is explicit and never implied by deletion.

- HTTP: `PUT /api/v1/universes/{universe}/configuration`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `universe` → `universe`
- Request body: JSON object: `displayName`, `metadata`, `protected`
- Access: `{"kind":"deployment"}`
- Group: `deployment/universes`
- Role: `none`

### `deployment/universes/expiry/put`

**Set or clear universe expiry**

Sets a future Unix timestamp or clears expiry with explicit null. Protected universes cannot have an expiry. Expiry starts the normal deletion process, attributed to the runtime; activity does not extend it.

- HTTP: `PUT /api/v1/universes/{universe}/expiry`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `universe` → `universe`
- Request body: JSON object: `expiresAtMs`
- Access: `{"kind":"deployment"}`
- Group: `deployment/universes`
- Role: `none`

### `deployment/universes/handle/put`

**Rename a universe**

Changes the deployment-unique handle without changing the universe UUID or resources. The previous handle is released: requests and URLs that use it stop reaching this universe, and another universe may claim it. Claiming another universe's handle returns a conflict. Does not create a universe.

- HTTP: `PUT /api/v1/universes/{universe}/handle`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `universe` → `universe`
- Request body: JSON object: `handle`
- Access: `{"kind":"deployment"}`
- Group: `deployment/universes`
- Role: `none`

### `deployment/universes/delete`

**Request universe deletion**

Refuses protected universes. Immediately fences an unprotected universe and releases its handle. A background worker removes schedules, workflows, environments, objects and rows. Repeated requests return deleting or deleted; unknown IDs return notFound.

- HTTP: `DELETE /api/v1/universes/{universe}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `universe` → `universe`
- Request body: none
- Access: `{"kind":"deployment"}`
- Group: `deployment/universes`
- Role: `none`

### `deployment/environment-providers/put`

**Put an environment provider**

Sets the controller endpoint, optional data route base and optional write-only bearer credential (kept when omitted, removed by clearCredential). Availability defaults to all universes on creation and is preserved when omitted on update. The controller is probed: a definitive refusal is invalidRequest, unreachable is accepted.

- HTTP: `PUT /api/v1/environment-providers/{providerId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `providerId` → `providerId`
- Request body: JSON object: `availableToAllUniverses`, `clearCredential`, `controllerEndpoint`, `credential`, `dataRouteBase`, `displayName`
- Access: `{"kind":"deployment"}`
- Group: `deployment/environment-providers`
- Role: `none`

### `deployment/environment-providers/list`

**List environment providers**

Returns every deployment-registered provider with its controller endpoint, data route base, and whether a credential is stored.

- HTTP: `GET /api/v1/environment-providers`
- Success: `200`
- Transfer: `json`
- Query: none
- Request body: none
- Access: `{"kind":"deployment"}`
- Group: `deployment/environment-providers`
- Role: `none`

### `deployment/environment-providers/read`

**Read an environment provider**

Returns one deployment-registered provider with its controller endpoint, data route base, and whether a credential is stored.

- HTTP: `GET /api/v1/environment-providers/{providerId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `providerId` → `providerId`
- Request body: none
- Access: `{"kind":"deployment"}`
- Group: `deployment/environment-providers`
- Role: `none`

### `deployment/environment-providers/delete`

**Delete an environment provider**

Deletes a deployment provider only when no universe binding and no non-closed environment references it.

- HTTP: `DELETE /api/v1/environment-providers/{providerId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `providerId` → `providerId`
- Request body: none
- Access: `{"kind":"deployment"}`
- Group: `deployment/environment-providers`
- Role: `none`

### `deployment/environment-providers/bindings/put`

**Put an environment provider binding**

Allows one universe to use a deployment provider that is not available to all universes. Repeating it changes nothing.

- HTTP: `PUT /api/v1/universes/{universe}/deployment-provider-bindings/{providerId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `universe` → `universe`, `providerId` → `providerId`
- Request body: empty JSON object or no body
- Access: `{"kind":"deployment"}`
- Group: `deployment/environment-providers`
- Role: `none`

### `deployment/environment-providers/bindings/delete`

**Delete an environment provider binding**

Stops a universe from using a provider that is not available to all universes. Its open environments on the provider lose access until it is bound again.

- HTTP: `DELETE /api/v1/universes/{universe}/deployment-provider-bindings/{providerId}`
- Success: `200`
- Transfer: `json`
- Query: none
- Path input fields: `universe` → `universe`, `providerId` → `providerId`
- Request body: none
- Access: `{"kind":"deployment"}`
- Group: `deployment/environment-providers`
- Role: `none`

