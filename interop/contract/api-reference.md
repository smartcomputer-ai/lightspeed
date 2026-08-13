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

### `session/managed/start`

**Create or reopen a managed session**

Creates a session with an immutable lifecycle controller and/or workflow tools using explicit bound pull/push dispatch, start targets, and Accepted, Joined, or keyed-Promise completion. Retrying an existing session id requires the same managed-creation declaration; an ordinary session cannot be upgraded to managed.

- Params: `ManagedSessionStartParams`
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

Applies a named or inline profile's config, instructions, and environment setup to an existing session; mutating profile sections require it to be open and idle. Pass current revisions to guard concurrent changes.

- Params: `ProfileApplyParams`
- Result: `AgentApiOutcome<ProfileApplyResponse>`

### `session/environments/activate`

**Activate a session environment**

Selects an allowed, live universe environment for environment-targeted tools while the session is idle.

- Params: `SessionEnvironmentActivateParams`
- Result: `AgentApiOutcome<SessionEnvironmentActivateResponse>`

### `session/environments/deactivate`

**Deactivate the session environment**

Clears active environment selection without changing or closing the universe environment.

- Params: `SessionEnvironmentDeactivateParams`
- Result: `AgentApiOutcome<SessionEnvironmentDeactivateResponse>`

### `environments/credentials/bind`

**Bind a credential into an environment**

Maps an environment variable name to an existing grant/provider/direct-secret handle for a universe environment. The response exposes only the source handle, never secret material.

- Params: `EnvironmentCredentialBindParams`
- Result: `AgentApiOutcome<EnvironmentCredentialBindResponse>`

### `environments/credentials/list`

**List environment credential bindings**

Returns variable names and credential source handles for a universe environment; resolved secret values are never returned.

- Params: `EnvironmentCredentialListParams`
- Result: `AgentApiOutcome<EnvironmentCredentialListResponse>`

### `environments/credentials/unbind`

**Unbind an environment credential**

Removes one variable-to-credential mapping without deleting the underlying grant, provider credential, or secret.

- Params: `EnvironmentCredentialUnbindParams`
- Result: `AgentApiOutcome<EnvironmentCredentialUnbindResponse>`

### `environments/create`

**Create an environment**

Records an idempotent provisioning intent against an enabled universe binding. The provider validates the template, entitlement, allocation, and capacity asynchronously.

- Params: `EnvironmentCreateParams`
- Result: `AgentApiOutcome<EnvironmentCreateResponse>`

### `environments/read`

**Read an environment**

Returns the durable universe resource, source binding, logical lifecycle state, and minimal current-incarnation identity.

- Params: `EnvironmentReadParams`
- Result: `AgentApiOutcome<EnvironmentReadResponse>`

### `environments/list`

**List environments**

Lists universe-owned environment resources, optionally filtered by provider, binding, or logical lifecycle state.

- Params: `EnvironmentListParams`
- Result: `AgentApiOutcome<EnvironmentListResponse>`

### `environments/close`

**Close an environment**

Records an asynchronous idempotent close intent. Provider cleanup is resumed by lifecycle reconciliation; quota is released only after Closed.

- Params: `EnvironmentCloseParams`
- Result: `AgentApiOutcome<EnvironmentCloseResponse>`

### `environments/external/create`

**Register an external environment**

Creates an environment backed by a Lightspeed-reachable envd WebSocket endpoint. Reachability is checked on demand.

- Params: `EnvironmentExternalCreateParams`
- Result: `AgentApiOutcome<EnvironmentExternalCreateResponse>`

### `environments/provider-bindings/list`

**List environment provider bindings**

Lists this universe's revisioned routing and admission bindings to deployment-scoped physical providers.

- Params: `EnvironmentProviderBindingListParams`
- Result: `AgentApiOutcome<EnvironmentProviderBindingListResponse>`

### `environments/provider-bindings/read`

**Read an environment provider binding**

Returns one universe routing and admission binding. Provider template entitlement, capacity, quota, and ingress policy remain provider-owned.

- Params: `EnvironmentProviderBindingReadParams`
- Result: `AgentApiOutcome<EnvironmentProviderBindingReadResponse>`

### `environments/templates/list`

**List environment templates**

Reads immutable templates directly from the selected bound provider controller.

- Params: `EnvironmentTemplateListParams`
- Result: `AgentApiOutcome<EnvironmentTemplateListResponse>`

### `environments/templates/read`

**Read an environment template**

Returns one immutable template version from the selected bound provider controller.

- Params: `EnvironmentTemplateReadParams`
- Result: `AgentApiOutcome<EnvironmentTemplateReadResponse>`

### `environments/jobs/create`

**Create environment jobs**

Starts a dependency-aware job group on one environment instance, injecting the environment's configured credentials at provider start. requestId is the retry identity; jobs are owned by the instance rather than a session.

- Params: `EnvironmentJobCreateParams`
- Result: `AgentApiOutcome<EnvironmentJobCreateResponse>`

### `environments/jobs/read`

**Read environment jobs**

Reads selected job handles with bounded output, optional sequence continuation, and optional artifacts; use returned status/sequence data for polling.

- Params: `EnvironmentJobReadParams`
- Result: `AgentApiOutcome<EnvironmentJobReadResponse>`

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

Stores the complete universe catalog document, including its optional universe auth-grant credential. Use expectedRevision when replacing; token material is never accepted or returned.

- Params: `McpServerPutParams`
- Result: `AgentApiOutcome<McpServerPutResponse>`

### `mcp/servers/read`

**Read an MCP server record**

Returns one catalog document with defaults, auth policy, non-secret grant binding, status, and revision; no credential value is exposed.

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

### `operator/environment-providers/put`

**Put an environment provider**

Registers or replaces one deployment provider and its controller connection. The provider does not call this API or require access to Lightspeed.

- Params: `OperatorEnvironmentProviderPutParams`
- Result: `AgentApiOutcome<OperatorEnvironmentProviderPutResponse>`

### `operator/environment-providers/list`

**List environment providers**

Returns every operator-registered deployment provider and its controller connection.

- Params: `OperatorEnvironmentProviderListParams`
- Result: `AgentApiOutcome<OperatorEnvironmentProviderListResponse>`

### `operator/environment-providers/read`

**Read an environment provider**

Returns one operator-registered deployment provider and its controller connection.

- Params: `OperatorEnvironmentProviderReadParams`
- Result: `AgentApiOutcome<OperatorEnvironmentProviderReadResponse>`

### `operator/environment-providers/delete`

**Delete an environment provider**

Deletes a deployment provider only when no universe binding references it.

- Params: `OperatorEnvironmentProviderDeleteParams`
- Result: `AgentApiOutcome<OperatorEnvironmentProviderDeleteResponse>`

### `operator/environment-providers/bindings/put`

**Put an environment provider binding**

Creates or replaces one universe's complete revisioned provider policy document. A deployment provider may have at most one binding in a universe.

- Params: `OperatorProviderBindingPutParams`
- Result: `AgentApiOutcome<OperatorProviderBindingPutResponse>`

### `operator/environment-providers/bindings/delete`

**Delete an environment provider binding**

Deletes a universe provider binding only after every referencing environment has reached Closed.

- Params: `OperatorProviderBindingDeleteParams`
- Result: `AgentApiOutcome<OperatorProviderBindingDeleteResponse>`

