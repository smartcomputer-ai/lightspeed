# Lightspeed JSON-RPC API Reference

Generated from the Rust API method manifest. Parameter and result field details live in `api.schema.json` and `openrpc.json`; this reference focuses on operation semantics.

## Universe methods

### `initialize`

**Inspect the Lightspeed protocol**

Returns protocol version, server identity, and supported capabilities without changing universe state.

- Params: `InitializeParams`
- Result: `AgentApiOutcome<InitializeResponse>`

### `session/start`

**Create or reopen a session**

Creates a session with optional config/profile setup. Retrying an existing session id returns that session; creation settings apply only when it is first created.

- Params: `SessionStartParams`
- Result: `AgentApiOutcome<SessionStartResponse>`

### `session/read`

**Read a session**

Returns the current projected session, including sparse config and revisions, lifecycle/run state, active context, and derived tools.

- Params: `SessionReadParams`
- Result: `AgentApiOutcome<SessionReadResponse>`

### `session/list`

**List sessions**

Returns a cursor-paginated summary list ordered by most recent update. Pages may shift while sessions are changing.

- Params: `SessionListParams`
- Result: `AgentApiOutcome<SessionListResponse>`

### `session/config/put`

**Replace session configuration**

Replaces the complete sparse config while the session is idle. Use the current config revision for safe read-modify-write; omitted features are revoked and an identical document is a no-op.

- Params: `SessionConfigPutParams`
- Result: `AgentApiOutcome<SessionConfigPutResponse>`

### `session/rename`

**Rename a session**

Sets the display name, or clears it when displayName is omitted.

- Params: `SessionRenameParams`
- Result: `AgentApiOutcome<SessionRenameResponse>`

### `session/close`

**Close a session**

Closes an idle session and detaches its environment bindings. Force mode cancels active work, drops queued runs, and can recover a session whose workflow is unavailable.

- Params: `SessionCloseParams`
- Result: `AgentApiOutcome<SessionCloseResponse>`

### `session/delete`

**Delete a closed session**

Permanently removes session storage after the session has been closed; close active/open sessions first.

- Params: `SessionDeleteParams`
- Result: `AgentApiOutcome<SessionDeleteResponse>`

### `session/events/read`

**Read the session event stream**

Reads events after a cursor and optionally long-polls when caught up. Continue from nextCursor/headCursor and inspect complete/gap rather than assuming an uninterrupted page.

- Params: `SessionEventsReadParams`
- Result: `AgentApiOutcome<SessionEventsReadResponse>`

### `session/context/append`

**Append keyed session context**

Admits a batch of context entries with per-entry results. Stable keys make same-content retries no-ops; media preprocessing can fail one entry without discarding successful entries.

- Params: `ContextAppendParams`
- Result: `AgentApiOutcome<ContextAppendResponse>`

### `session/context/remove`

**Remove keyed session context**

Removes active entries by stable key with per-key results. Missing keys are idempotent no-ops; runtime-reserved run keys cannot be removed.

- Params: `ContextRemoveParams`
- Result: `AgentApiOutcome<ContextRemoveResponse>`

### `session/context/compact`

**Compact session context**

Runs the configured compaction policy on an open idle session and waits for the resulting context revision.

- Params: `ContextCompactParams`
- Result: `AgentApiOutcome<ContextCompactResponse>`

### `session/runs/start`

**Start an agent run**

Accepts input or existing context keys and returns once the run is queued/accepted, not when it finishes. Supply submissionId for retry safety, then follow session events or reread the session.

- Params: `RunStartParams`
- Result: `AgentApiOutcome<RunStartResponse>`

### `session/runs/cancel`

**Cancel a run**

Requests cancellation of the named queued or active run and returns its current projected state; observe session events for terminal completion.

- Params: `RunCancelParams`
- Result: `AgentApiOutcome<RunCancelResponse>`

### `session/skills/list`

**List available session skills**

Refreshes the session's configured VFS skill catalog and reports which discovered skills are enabled and active. An absent catalog yields an empty result.

- Params: `SkillListParams`
- Result: `AgentApiOutcome<SkillListResponse>`

### `session/skills/active`

**List active session skills**

Returns skill instructions currently injected into context, including activation scope and source.

- Params: `SkillActiveParams`
- Result: `AgentApiOutcome<SkillActiveResponse>`

### `session/skills/activate`

**Activate a session skill**

Loads an enabled skill from the current catalog and injects its instructions into an open idle session. Run-scoped activation is the default.

- Params: `SkillActivateParams`
- Result: `AgentApiOutcome<SkillActivateResponse>`

### `session/skills/deactivate`

**Deactivate a session skill**

Removes an active skill's injected context from an open idle session; the skill must currently be active.

- Params: `SkillDeactivateParams`
- Result: `AgentApiOutcome<SkillDeactivateResponse>`

### `session/profiles/apply`

**Apply a profile to a session**

Applies a named or inline profile's config, instructions, mounts, and environment setup to an existing session; mutating profile sections require it to be open and idle. Pass current revisions to guard concurrent changes.

- Params: `ProfileApplyParams`
- Result: `AgentApiOutcome<ProfileApplyResponse>`

### `session/mounts/put`

**Create or replace a session mount**

Binds a snapshot or workspace at a path on an open idle session that grants VFS. Workspace mounts follow that workspace's current head.

- Params: `VfsMountPutParams`
- Result: `AgentApiOutcome<VfsMountPutResponse>`

### `session/mounts/list`

**List session mounts**

Returns the session's snapshot/workspace bindings and access modes.

- Params: `VfsMountListParams`
- Result: `AgentApiOutcome<VfsMountListResponse>`

### `session/mounts/delete`

**Delete a session mount**

Removes a binding from an open idle session without deleting its source snapshot or workspace.

- Params: `VfsMountDeleteParams`
- Result: `AgentApiOutcome<VfsMountDeleteResponse>`

### `session/environments/read`

**Read a session environment binding**

Returns one session-local environment alias joined with current instance/provider availability and activation state.

- Params: `SessionEnvironmentReadParams`
- Result: `AgentApiOutcome<SessionEnvironmentReadResponse>`

### `session/environments/list`

**List session environment bindings**

Returns all environment aliases attached to the session and identifies the active tool target, if any.

- Params: `SessionEnvironmentListParams`
- Result: `AgentApiOutcome<SessionEnvironmentListResponse>`

### `session/environments/attach`

**Attach an environment to a session**

Binds an existing universe environment instance under a session-local alias, optionally activating it. The session must grant environments and allow the provider.

- Params: `SessionEnvironmentAttachParams`
- Result: `AgentApiOutcome<SessionEnvironmentAttachResponse>`

### `session/environments/activate`

**Activate a session environment**

Selects an attached, available environment as the process/filesystem tool target while the session is idle.

- Params: `SessionEnvironmentActivateParams`
- Result: `AgentApiOutcome<SessionEnvironmentActivateResponse>`

### `session/environments/deactivate`

**Deactivate the session environment**

Clears the active environment tool target without detaching any binding or closing the underlying instance.

- Params: `SessionEnvironmentDeactivateParams`
- Result: `AgentApiOutcome<SessionEnvironmentDeactivateResponse>`

### `session/environments/detach`

**Detach a session environment**

Detaches the session-local binding; detaching the active target requires an idle session and deactivates it first. The universe instance and jobs remain independently owned.

- Params: `SessionEnvironmentDetachParams`
- Result: `AgentApiOutcome<SessionEnvironmentDetachResponse>`

### `session/environments/credentials/bind`

**Bind a credential into an environment**

Maps an environment variable name to an existing grant/provider/direct-secret handle for one session binding. The response exposes only the source handle, never secret material.

- Params: `SessionEnvironmentCredentialBindParams`
- Result: `AgentApiOutcome<SessionEnvironmentCredentialBindResponse>`

### `session/environments/credentials/list`

**List environment credential bindings**

Returns variable names and credential source handles for a session environment; resolved secret values are never returned.

- Params: `SessionEnvironmentCredentialListParams`
- Result: `AgentApiOutcome<SessionEnvironmentCredentialListResponse>`

### `session/environments/credentials/unbind`

**Unbind an environment credential**

Removes one variable-to-credential mapping without deleting the underlying grant, provider credential, or secret.

- Params: `SessionEnvironmentCredentialUnbindParams`
- Result: `AgentApiOutcome<SessionEnvironmentCredentialUnbindResponse>`

### `environments/create`

**Provision an environment instance**

Asks a live provider with create capability to create a universe-owned environment instance. This does not attach the instance to any session.

- Params: `EnvironmentCreateParams`
- Result: `AgentApiOutcome<EnvironmentCreateResponse>`

### `environments/read`

**Read an environment instance**

Returns the universe resource with its provider identity, current observed lifecycle, connection, scope, and capabilities.

- Params: `EnvironmentReadParams`
- Result: `AgentApiOutcome<EnvironmentReadResponse>`

### `environments/list`

**List environment instances**

Lists universe-owned instances, optionally filtered by provider or observed target status.

- Params: `EnvironmentListParams`
- Result: `AgentApiOutcome<EnvironmentListResponse>`

### `environments/close`

**Close an environment instance**

Tears down the universe resource through its provider. Closing is rejected while session bindings or nonterminal jobs still occupy the instance.

- Params: `EnvironmentCloseParams`
- Result: `AgentApiOutcome<EnvironmentCloseResponse>`

### `environments/jobs/create`

**Create environment jobs**

Starts a dependency-aware job group on one environment instance. requestId is the retry identity; jobs are owned by the instance rather than a session.

- Params: `EnvironmentJobCreateParams`
- Result: `AgentApiOutcome<EnvironmentJobCreateResponse>`

### `environments/jobs/read`

**Read environment jobs**

Reads selected job handles with bounded output, optional sequence continuation, and optional artifacts; use returned status/sequence data for polling.

- Params: `EnvironmentJobReadParams`
- Result: `AgentApiOutcome<EnvironmentJobReadResponse>`

### `environments/jobs/list`

**List environment jobs**

Lists durable job records across the universe, optionally narrowed to an instance or job group.

- Params: `EnvironmentJobListParams`
- Result: `AgentApiOutcome<EnvironmentJobListResponse>`

### `environments/jobs/cancel`

**Cancel environment jobs**

Requests cancellation for selected jobs, optionally including dependents. Force is provider-specific escalation; inspect each per-job result.

- Params: `EnvironmentJobCancelParams`
- Result: `AgentApiOutcome<EnvironmentJobCancelResponse>`

### `models/list`

**Discover available models**

Queries supported providers directly on every call and returns best-effort selectable routes. One provider failure does not discard successful results from others.

- Params: `ModelListParams`
- Result: `AgentApiOutcome<ModelListResponse>`

### `profiles/create`

**Create an agent profile**

Creates a new universe-scoped reusable profile document; use profiles/put for create-or-replace revision semantics.

- Params: `ProfileCreateParams`
- Result: `AgentApiOutcome<ProfileCreateResponse>`

### `profiles/read`

**Read an agent profile**

Returns the complete profile document and current revision.

- Params: `ProfileReadParams`
- Result: `AgentApiOutcome<ProfileReadResponse>`

### `profiles/list`

**List agent profiles**

Returns lightweight summaries of universe-scoped reusable profiles.

- Params: `ProfileListParams`
- Result: `AgentApiOutcome<ProfileListResponse>`

### `profiles/put`

**Create or replace an agent profile**

Stores the complete profile document. Use expectedRevision from profiles/read when replacing to prevent lost updates; absence writes unconditionally.

- Params: `ProfilePutParams`
- Result: `AgentApiOutcome<ProfilePutResponse>`

### `profiles/delete`

**Delete an agent profile**

Deletes the catalog document; sessions previously created or configured from it retain their materialized state.

- Params: `ProfileDeleteParams`
- Result: `AgentApiOutcome<ProfileDeleteResponse>`

### `blobs/put`

**Store content-addressed blobs**

Decodes and stores a batch of base64 payloads, returning immutable content references in request order. Re-uploading identical bytes is naturally deduplicated.

- Params: `BlobPutParams`
- Result: `AgentApiOutcome<BlobPutResponse>`

### `blobs/read`

**Read a content-addressed blob**

Returns the complete immutable blob as base64; large values count against gateway and MCP response limits.

- Params: `BlobReadParams`
- Result: `AgentApiOutcome<BlobReadResponse>`

### `blobs/has`

**Check blob availability**

Checks a batch of content references without returning blob bodies, preserving request order.

- Params: `BlobHasParams`
- Result: `AgentApiOutcome<BlobHasResponse>`

### `vfs/snapshots/commit`

**Commit a VFS snapshot**

Validates and stores an immutable filesystem manifest. Upload referenced file blobs first; the returned snapshot ref is content-addressed.

- Params: `VfsSnapshotCommitParams`
- Result: `AgentApiOutcome<VfsSnapshotCommitResponse>`

### `vfs/snapshots/read`

**Read a VFS snapshot**

Returns an immutable snapshot manifest and aggregate file/byte counts; file bodies remain separate blobs.

- Params: `VfsSnapshotReadParams`
- Result: `AgentApiOutcome<VfsSnapshotReadResponse>`

### `vfs/workspaces/create`

**Create a mutable VFS workspace**

Creates a universe workspace at an optional seed snapshot; absence starts from a server-created empty snapshot.

- Params: `VfsWorkspaceCreateParams`
- Result: `AgentApiOutcome<VfsWorkspaceCreateResponse>`

### `vfs/workspaces/read`

**Read a VFS workspace**

Returns workspace metadata, current head snapshot, and revision for safe updates.

- Params: `VfsWorkspaceReadParams`
- Result: `AgentApiOutcome<VfsWorkspaceReadResponse>`

### `vfs/workspaces/list`

**List VFS workspaces**

Lists mutable universe workspaces with head snapshots, sizes, and revisions.

- Params: `VfsWorkspaceListParams`
- Result: `AgentApiOutcome<VfsWorkspaceListResponse>`

### `vfs/workspaces/update`

**Update a VFS workspace**

Moves the workspace head to an existing snapshot and updates its display name. Pass expectedRevision from a read to prevent lost updates.

- Params: `VfsWorkspaceUpdateParams`
- Result: `AgentApiOutcome<VfsWorkspaceUpdateResponse>`

### `vfs/workspaces/delete`

**Delete a VFS workspace**

Deletes the mutable workspace record; immutable snapshots and blobs remain content-addressed resources.

- Params: `VfsWorkspaceDeleteParams`
- Result: `AgentApiOutcome<VfsWorkspaceDeleteResponse>`

### `mcp/servers/put`

**Create or replace an MCP server record**

Stores the complete universe catalog document. Use expectedRevision when replacing; authenticated policies reference grants but never embed credentials.

- Params: `McpServerPutParams`
- Result: `AgentApiOutcome<McpServerPutResponse>`

### `mcp/servers/read`

**Read an MCP server record**

Returns one catalog document with defaults, auth policy, status, and revision; no credential value is exposed.

- Params: `McpServerReadParams`
- Result: `AgentApiOutcome<McpServerReadResponse>`

### `mcp/servers/list`

**List MCP server records**

Lists universe catalog entries, optionally filtered by lifecycle/configuration status.

- Params: `McpServerListParams`
- Result: `AgentApiOutcome<McpServerListResponse>`

### `mcp/servers/delete`

**Delete an MCP server record**

Deletes the catalog document. Existing session configs that reference it are not silently rewritten and may need explicit reconfiguration.

- Params: `McpServerDeleteParams`
- Result: `AgentApiOutcome<McpServerDeleteResponse>`

### `environments/providers/register`

**Register environment provider presence**

Publishes a controller endpoint, capabilities, implementation identity, and liveness lease. Intended for trusted provider/bridge infrastructure, not ordinary configuration clients.

- Params: `EnvironmentProviderRegisterParams`
- Result: `AgentApiOutcome<EnvironmentProviderRegisterResponse>`

### `environments/providers/heartbeat`

**Refresh environment provider presence**

Renews a provider lease and records its complete observed target descriptors. Omitted provided targets may become unknown; intended for provider infrastructure.

- Params: `EnvironmentProviderHeartbeatParams`
- Result: `AgentApiOutcome<EnvironmentProviderHeartbeatResponse>`

### `environments/providers/unregister`

**Unregister environment provider presence**

Marks provider presence offline without deleting its durable environment instance records.

- Params: `EnvironmentProviderUnregisterParams`
- Result: `AgentApiOutcome<EnvironmentProviderUnregisterResponse>`

### `environments/providers/list`

**List environment providers**

Lists current provider presence with lease-derived online/stale/offline status, optionally filtered by status or kind.

- Params: `EnvironmentProviderListParams`
- Result: `AgentApiOutcome<EnvironmentProviderListResponse>`

### `auth/grants/import`

**Import a static bearer grant**

Accepts a plaintext token, encrypts it immediately, and returns only grant metadata/token-presence flags. The token can never be read back through the API.

- Params: `AuthGrantImportParams`
- Result: `AgentApiOutcome<AuthGrantImportResponse>`

### `auth/grants/read`

**Read authentication grant metadata**

Returns principal, provider binding, scopes, audience, expiry, status, and token-presence flags; access and refresh token values are never returned.

- Params: `AuthGrantReadParams`
- Result: `AgentApiOutcome<AuthGrantReadResponse>`

### `auth/grants/list`

**List authentication grants**

Lists non-secret grant metadata for the universe, optionally filtered by status.

- Params: `AuthGrantListParams`
- Result: `AgentApiOutcome<AuthGrantListResponse>`

### `auth/grants/revoke`

**Revoke an authentication grant**

Marks the grant unusable by token consumers while retaining non-secret audit metadata.

- Params: `AuthGrantRevokeParams`
- Result: `AgentApiOutcome<AuthGrantRevokeResponse>`

### `auth/clients/create`

**Register an OAuth client**

Stores provider endpoints and client identity; an optional plaintext client secret is encrypted and represented thereafter only by hasClientSecret.

- Params: `AuthClientCreateParams`
- Result: `AgentApiOutcome<AuthClientCreateResponse>`

### `auth/clients/read`

**Read OAuth client metadata**

Returns endpoints, public client identity, defaults, and secret-presence state; the client secret is never returned.

- Params: `AuthClientReadParams`
- Result: `AgentApiOutcome<AuthClientReadResponse>`

### `auth/clients/list`

**List OAuth clients**

Lists non-secret OAuth client registrations in the universe.

- Params: `AuthClientListParams`
- Result: `AgentApiOutcome<AuthClientListResponse>`

### `auth/clients/delete`

**Delete an OAuth client**

Deletes the client registration and its stored client secret; grants already created from it remain separate records.

- Params: `AuthClientDeleteParams`
- Result: `AgentApiOutcome<AuthClientDeleteResponse>`

### `auth/flows/start`

**Start an OAuth authorization flow**

Creates a short-lived PKCE flow and returns a browser authorization URL containing one-time state. Treat the URL as sensitive and poll auth/flows/read for completion.

- Params: `AuthFlowStartParams`
- Result: `AgentApiOutcome<AuthFlowStartResponse>`

### `auth/flows/read`

**Read OAuth flow status**

Polls a flow's pending/completed/failed/expired state and returns the resulting grant id when authorization succeeds; no token value is exposed.

- Params: `AuthFlowStatusParams`
- Result: `AgentApiOutcome<AuthFlowStatusResponse>`

### `auth/providers/create`

**Register an authentication provider**

Creates a model or GitHub credential source. Plaintext API keys/private keys are encrypted on receipt and later represented only by configuration plus hasCredential.

- Params: `AuthProviderCreateParams`
- Result: `AgentApiOutcome<AuthProviderCreateResponse>`

### `auth/providers/read`

**Read authentication provider metadata**

Returns provider kind, non-secret configuration, credential-presence state, and status; stored credentials are never returned.

- Params: `AuthProviderReadParams`
- Result: `AgentApiOutcome<AuthProviderReadResponse>`

### `auth/providers/list`

**List authentication providers**

Lists non-secret model/GitHub provider registrations for the universe.

- Params: `AuthProviderListParams`
- Result: `AgentApiOutcome<AuthProviderListResponse>`

### `auth/providers/delete`

**Delete an authentication provider**

Deletes the provider registration and its directly stored credential; separately stored grants remain independent records.

- Params: `AuthProviderDeleteParams`
- Result: `AgentApiOutcome<AuthProviderDeleteResponse>`

### `auth/github/installations/list`

**List GitHub App installations**

Uses the registered GitHub App provider credential to query accessible installations and returns account/permission metadata without tokens.

- Params: `AuthGitHubInstallationListParams`
- Result: `AgentApiOutcome<AuthGitHubInstallationListResponse>`

### `auth/github/installations/grant`

**Grant access to a GitHub App installation**

Creates or refreshes a universe auth grant for one accessible installation. The installation token is brokered internally and never returned.

- Params: `AuthGitHubInstallationGrantParams`
- Result: `AgentApiOutcome<AuthGitHubInstallationGrantResponse>`

### `outbox/read`

**Read pending outbound messages**

Cursor-reads or long-polls the universe delivery outbox. Advance with nextAfter, but only outbox/ack marks individual entries delivered or failed.

- Params: `OutboxReadParams`
- Result: `AgentApiOutcome<OutboxReadResponse>`

### `outbox/ack`

**Acknowledge outbound delivery**

Records delivered or failed delivery for one outbox entry and updates attempt/status state. Intended for messaging delivery workers.

- Params: `OutboxAckParams`
- Result: `AgentApiOutcome<OutboxAckResponse>`


## Operator methods

### `operator/universes/create`

**Create a universe**

Creates the deployment tenant boundary for an explicit UUID. The operation is idempotent and reports whether a new universe was created.

- Params: `OperatorUniverseCreateParams`
- Result: `AgentApiOutcome<OperatorUniverseCreateResponse>`

### `operator/universes/list`

**List universes**

Returns deployment-wide universe summaries with approximate live aggregate counts and last session activity.

- Params: `OperatorUniverseListParams`
- Result: `AgentApiOutcome<OperatorUniverseListResponse>`

### `operator/universes/read`

**Read a universe**

Returns one deployment tenant summary with aggregate session, workspace, profile, and blob usage.

- Params: `OperatorUniverseReadParams`
- Result: `AgentApiOutcome<OperatorUniverseReadResponse>`

### `operator/universes/delete`

**Purge a universe**

Permanently terminates live session workflows, deletes external blob objects, and cascades universe data. The purge is resumable/idempotent after partial failure.

- Params: `OperatorUniverseDeleteParams`
- Result: `AgentApiOutcome<OperatorUniverseDeleteResponse>`

### `operator/api-keys/create`

**Create a universe API key**

Mints an inbound gateway key for one existing universe. The plaintext secret is returned exactly once and cannot be recovered; persist only the displayed prefix for identification.

- Params: `OperatorApiKeyCreateParams`
- Result: `AgentApiOutcome<OperatorApiKeyCreateResponse>`

### `operator/api-keys/list`

**List universe API keys**

Returns only non-secret key metadata for the requested universe, including revocation and last-use timestamps. Plaintext secrets are never stored or returned.

- Params: `OperatorApiKeyListParams`
- Result: `AgentApiOutcome<OperatorApiKeyListResponse>`

### `operator/api-keys/revoke`

**Revoke a universe API key**

Immediately and idempotently revokes the matching key only when it belongs to the requested universe. Unknown and foreign-universe prefixes return not found.

- Params: `OperatorApiKeyRevokeParams`
- Result: `AgentApiOutcome<OperatorApiKeyRevokeResponse>`

### `operator/outbox/read`

**Read the deployment outbox**

Cursor-reads or long-polls pending messages across all universes. Entries identify their universe; acknowledge each through universe-scoped outbox/ack.

- Params: `OperatorOutboxReadParams`
- Result: `AgentApiOutcome<OperatorOutboxReadResponse>`

