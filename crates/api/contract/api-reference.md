# Lightspeed JSON-RPC API Reference

Generated from the Rust API method manifest. Parameter and result field details live in `api.schema.json` and `openrpc.json`; this reference focuses on operation semantics.

A key calls a method when it holds the method's group. Role and target are metadata for gates that decide for people, such as the Platform; core does not evaluate them.

## Universe methods

### `vfs/workspaces/files/read`

**Read a workspace file**

Reads bytes at a path in the current workspace head.

- Access: `{"kind":"universe","action":"read"}`
- Group: `vfs`
- Role: `viewer`
- Target: `none`
- Params: `VfsWorkspaceFileReadParams`
- Result: `AgentApiOutcome<BlobReadResponse>`

### `initialize`

**Inspect the Lightspeed protocol**

Returns protocol version, server identity, and supported capabilities without changing universe state.

- Access: `{"kind":"universe","action":"read"}`
- Group: `none`
- Role: `viewer`
- Target: `none`
- Params: `InitializeParams`
- Result: `AgentApiOutcome<InitializeResponse>`

### `session/start`

**Create or reopen a session**

Creates a session, unshared unless access says universe, with optional config/profile setup. Profile metadata and retention supply defaults that explicit values override; the config's default environment attachment becomes active. Retrying an existing id returns that session and keeps its audience.

- Access: `{"kind":"universe","action":"create_session"}`
- Group: `session`
- Role: `contributor`
- Target: `sessionId`
- Params: `SessionStartParams`
- Result: `AgentApiOutcome<SessionStartResponse>`

### `session/managed/start`

**Create or reopen a managed session**

Creates a session with immutable lifecycle and workflow-tool declarations using explicit bound dispatch. Profile metadata, retention, and default environment attachment selection follow session/start semantics. Retrying an id requires the same managed declaration; an ordinary session cannot be upgraded.

- Access: `{"kind":"universe","action":"create_session"}`
- Group: `session`
- Role: `contributor`
- Target: `sessionId`
- Params: `ManagedSessionStartParams`
- Result: `AgentApiOutcome<SessionStartResponse>`

### `session/read`

**Read a session**

Returns current state plus a bounded newest-first run-summary page. Follow nextRunCursor with session/runs/list when hasOlderRuns is true; use session/events/read for the transcript.

- Access: `{"kind":"universe","action":"read"}`
- Group: `session`
- Role: `viewer`
- Target: `sessionId`
- Params: `SessionReadParams`
- Result: `AgentApiOutcome<SessionReadResponse>`

### `session/list`

**List sessions**

Returns a cursor-paginated summary list ordered by most recent update, optionally narrowed by the audience of each session's root: createdBy, visibility, or visibleTo (shared with the universe or created by that actor). Pages may shift while sessions are changing.

- Access: `{"kind":"universe","action":"read"}`
- Group: `session`
- Role: `viewer`
- Target: `none`
- Params: `SessionListParams`
- Result: `AgentApiOutcome<SessionListResponse>`

### `session/config/put`

**Replace session configuration**

Replaces the complete sparse config while the session is idle. Use the current config revision for safe read-modify-write; omitted features are revoked and an identical document is a no-op.

- Access: `{"kind":"universe","action":"control_session"}`
- Group: `session`
- Role: `contributor`
- Target: `sessionId`
- Params: `SessionConfigPutParams`
- Result: `AgentApiOutcome<SessionConfigPutResponse>`

### `session/rename`

**Rename a session**

Sets the display name, or clears it when displayName is omitted.

- Access: `{"kind":"universe","action":"control_session"}`
- Group: `session`
- Role: `contributor`
- Target: `sessionId`
- Params: `SessionRenameParams`
- Result: `AgentApiOutcome<SessionRenameResponse>`

### `session/metadata/put`

**Replace session metadata**

Replaces the complete descriptive key/value map (bounded like session/start); an omitted or empty map clears it. Record-only: the event log and updatedAtMs are untouched.

- Access: `{"kind":"universe","action":"control_session"}`
- Group: `session`
- Role: `contributor`
- Target: `sessionId`
- Params: `SessionMetadataPutParams`
- Result: `AgentApiOutcome<SessionMetadataPutResponse>`

### `session/retention/put`

**Replace session retention**

Sets the positive close-relative automatic-deletion duration on a retention root, or clears it with null. Forks and delegated children inherit the root policy and cannot override it.

- Access: `{"kind":"universe","action":"control_session"}`
- Group: `session`
- Role: `contributor`
- Target: `sessionId`
- Params: `SessionRetentionPutParams`
- Result: `AgentApiOutcome<SessionRetentionPutResponse>`

### `session/close`

**Close a session**

Closes an idle session and detaches its environment bindings. Force mode cancels active work, drops queued runs, and can recover a session whose workflow is unavailable.

- Access: `{"kind":"universe","action":"stop_session"}`
- Group: `session`
- Role: `contributor`
- Target: `sessionId`
- Params: `SessionCloseParams`
- Result: `AgentApiOutcome<SessionCloseResponse>`

### `session/delete`

**Delete closed sessions**

Permanently removes a closed retention-tree leaf, or its closed history-fork and delegated-child subtree when cascade is true. Config-only clones are never included.

- Access: `{"kind":"universe","action":"delete_session"}`
- Group: `session`
- Role: `contributor`
- Target: `sessionId`
- Params: `SessionDeleteParams`
- Result: `AgentApiOutcome<SessionDeleteResponse>`

### `session/share`

**Share a session with the universe**

Moves an unshared root session to universe visibility, one way; its delegated children follow it. Refused on a bot's session, a delegated child, and a session already shared. Core applies it for any caller of the method; who may share is the caller's gate's decision.

- Access: `{"kind":"universe","action":"share_session"}`
- Group: `session`
- Role: `contributor`
- Target: `sessionId`
- Params: `SessionShareParams`
- Result: `AgentApiOutcome<SessionShareResponse>`

### `session/events/read`

**Read the session event stream**

Returns chronological events. Forward (default) follows after and supports long-polling. Backward reads the latest window below before (or the head); pass nextCursor as before until complete. Follow live events after the initial backward headCursor. Windows may split runs/tool batches; keep historical reconstruction separate from live controls.

- Access: `{"kind":"universe","action":"read"}`
- Group: `session`
- Role: `viewer`
- Target: `sessionId`
- Params: `SessionEventsReadParams`
- Result: `AgentApiOutcome<SessionEventsReadResponse>`

### `session/context/append`

**Append keyed session context**

Admits a batch of context entries with per-entry results. Stable keys make same-content retries no-ops; media preprocessing can fail one entry without discarding successful entries.

- Access: `{"kind":"universe","action":"control_session"}`
- Group: `session`
- Role: `contributor`
- Target: `sessionId`
- Params: `ContextAppendParams`
- Result: `AgentApiOutcome<ContextAppendResponse>`

### `session/context/remove`

**Remove keyed session context**

Removes active entries by stable key with per-key results. Missing keys are idempotent no-ops; runtime-reserved run keys cannot be removed.

- Access: `{"kind":"universe","action":"control_session"}`
- Group: `session`
- Role: `contributor`
- Target: `sessionId`
- Params: `ContextRemoveParams`
- Result: `AgentApiOutcome<ContextRemoveResponse>`

### `session/context/compact`

**Compact session context**

Runs the configured compaction policy on an open idle session and waits for the resulting context revision.

- Access: `{"kind":"universe","action":"control_session"}`
- Group: `session`
- Role: `contributor`
- Target: `sessionId`
- Params: `ContextCompactParams`
- Result: `AgentApiOutcome<ContextCompactResponse>`

### `session/runs/start`

**Start an agent run**

Accepts input or existing context keys and returns once the run is accepted — queued behind an active run, or running — not when it finishes. Supply submissionId for retry safety, then follow session events or reread the session.

- Access: `{"kind":"universe","action":"control_session"}`
- Group: `session`
- Role: `contributor`
- Target: `sessionId`
- Params: `RunStartParams`
- Result: `AgentApiOutcome<RunStartResponse>`

### `session/runs/list`

**List session runs**

Returns a newest-first keyset page of bounded run summaries projected from current reducer state.

- Access: `{"kind":"universe","action":"read"}`
- Group: `session`
- Role: `viewer`
- Target: `sessionId`
- Params: `RunListParams`
- Result: `AgentApiOutcome<RunListResponse>`

### `session/runs/read`

**Read one session run**

Reads and projects one run from its bounded event interval, paged by event sequence.

- Access: `{"kind":"universe","action":"read"}`
- Group: `session`
- Role: `viewer`
- Target: `sessionId`
- Params: `RunReadParams`
- Result: `AgentApiOutcome<RunReadResponse>`

### `session/runs/cancel`

**Cancel a run**

Requests cancellation of the named queued or active run and returns its current projected state; observe session events for terminal completion. In-flight model and tool activity is aborted; no grace turn runs.

- Access: `{"kind":"universe","action":"stop_session"}`
- Group: `session`
- Role: `contributor`
- Target: `sessionId`
- Params: `RunCancelParams`
- Result: `AgentApiOutcome<RunCancelResponse>`

### `session/runs/approvals/decide`

**Decide pending run approvals**

Approves or rejects pending MCP tool calls on the named active run. Valid decisions apply independently; the run resumes only after every pending approval has a decision.

- Access: `{"kind":"universe","action":"control_session"}`
- Group: `session`
- Role: `contributor`
- Target: `sessionId`
- Params: `RunApprovalsDecideParams`
- Result: `AgentApiOutcome<RunApprovalsDecideResponse>`

### `session/runs/steer`

**Steer the active run**

Injects input into the named active run; the model sees it at the next turn boundary without interrupting the in-flight turn. Accepted while the run is running or parked on an await; rejected for queued, cancelling, or finished runs.

- Access: `{"kind":"universe","action":"control_session"}`
- Group: `session`
- Role: `contributor`
- Target: `sessionId`
- Params: `RunSteerParams`
- Result: `AgentApiOutcome<RunSteerResponse>`

### `session/skills/list`

**List available session skills**

Returns separate VFS and environment catalogs with source, reference, availability, readable skill paths, and warnings. Refreshes only when open with no active or queued run, without waking environments. Absent catalogs are omitted.

- Access: `{"kind":"universe","action":"read"}`
- Group: `session`
- Role: `viewer`
- Target: `sessionId`
- Params: `SkillListParams`
- Result: `AgentApiOutcome<SkillListResponse>`

### `session/profiles/apply`

**Apply a profile to a session**

Applies a named or inline profile's config, instructions, and environment setup to an existing session; mutating profile sections require it to be open and idle. Pass current revisions to guard concurrent changes.

- Access: `{"kind":"universe","action":"control_session"}`
- Group: `session`
- Role: `contributor`
- Target: `sessionId`
- Params: `ProfileApplyParams`
- Result: `AgentApiOutcome<ProfileApplyResponse>`

### `session/environments/activate`

**Activate a session environment**

Selects an attached, live universe environment for environment-targeted tools while the session is idle.

- Access: `{"kind":"universe","action":"control_session"}`
- Group: `session`
- Role: `contributor`
- Target: `sessionId`
- Params: `SessionEnvironmentActivateParams`
- Result: `AgentApiOutcome<SessionEnvironmentActivateResponse>`

### `session/environments/deactivate`

**Deactivate the session environment**

Clears active environment selection without changing or closing the universe environment.

- Access: `{"kind":"universe","action":"control_session"}`
- Group: `session`
- Role: `contributor`
- Target: `sessionId`
- Params: `SessionEnvironmentDeactivateParams`
- Result: `AgentApiOutcome<SessionEnvironmentDeactivateResponse>`

### `environments/credentials/bind`

**Bind a credential into an environment**

Maps an environment variable name to an existing grant/provider/direct-secret handle for a universe environment. Requires configuring the environment and configuring resources in the universe. The response exposes only the source handle, never secret material.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `environments`
- Role: `operator`
- Target: `none`
- Params: `EnvironmentCredentialBindParams`
- Result: `AgentApiOutcome<EnvironmentCredentialBindResponse>`

### `environments/credentials/list`

**List environment credential bindings**

Returns variable names and credential source handles for a universe environment; resolved secret values are never returned.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `environments`
- Role: `operator`
- Target: `none`
- Params: `EnvironmentCredentialListParams`
- Result: `AgentApiOutcome<EnvironmentCredentialListResponse>`

### `environments/credentials/unbind`

**Unbind an environment credential**

Removes one variable-to-credential mapping without deleting the underlying grant, provider credential, or secret.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `environments`
- Role: `operator`
- Target: `none`
- Params: `EnvironmentCredentialUnbindParams`
- Result: `AgentApiOutcome<EnvironmentCredentialUnbindResponse>`

### `environments/create`

**Create an environment**

Records an idempotent provisioning intent against an enabled universe binding, attributed to the caller. The provider validates its provider-wide template and provisions through its backend asynchronously.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `environments`
- Role: `operator`
- Target: `none`
- Params: `EnvironmentCreateParams`
- Result: `AgentApiOutcome<EnvironmentCreateResponse>`

### `environments/read`

**Read an environment**

Returns the durable universe resource, source binding, logical lifecycle state, and minimal current-incarnation identity.

- Access: `{"kind":"universe","action":"read"}`
- Group: `environments`
- Role: `viewer`
- Target: `none`
- Params: `EnvironmentReadParams`
- Result: `AgentApiOutcome<EnvironmentReadResponse>`

### `environments/list`

**List environments**

Lists the universe environments, optionally filtered by provider, binding, or logical lifecycle state.

- Access: `{"kind":"universe","action":"read"}`
- Group: `environments`
- Role: `viewer`
- Target: `none`
- Params: `EnvironmentListParams`
- Result: `AgentApiOutcome<EnvironmentListResponse>`

### `environments/close`

**Close an environment**

Records an asynchronous idempotent close intent. Provider cleanup is resumed by lifecycle reconciliation; quota is released only after Closed.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `environments`
- Role: `operator`
- Target: `none`
- Params: `EnvironmentCloseParams`
- Result: `AgentApiOutcome<EnvironmentCloseResponse>`

### `environments/external/create`

**Register an external environment**

Creates an environment backed by a Lightspeed-reachable envd WebSocket endpoint, attributed to the caller. Reachability is checked on demand.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `environments`
- Role: `operator`
- Target: `none`
- Params: `EnvironmentExternalCreateParams`
- Result: `AgentApiOutcome<EnvironmentExternalCreateResponse>`

### `environments/ingress/put`

**Configure environment public ingress**

Synchronously enables or disables one provider-authorized HTTPS endpoint for a provisioned environment. The provider owns hostname allocation, the approved guest port, routing, TLS, and health.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `environments`
- Role: `operator`
- Target: `none`
- Params: `EnvironmentIngressPutParams`
- Result: `AgentApiOutcome<EnvironmentIngressPutResponse>`

### `environments/power/put`

**Set environment power intent**

Records the desired power state (running, paused, suspended, or stopped) of a provisioned environment; the lifecycle reconciler converges the provider target asynchronously. Powered-down environments wake transparently on their next use. Rejected when the provider does not support the state.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `environments`
- Role: `operator`
- Target: `none`
- Params: `EnvironmentPowerPutParams`
- Result: `AgentApiOutcome<EnvironmentPowerPutResponse>`

### `environments/idle-policy/put`

**Set environment idle policy**

Replaces or clears the staged idle policy of a provisioned environment. The power reaper measures the daemon's idle duration against the pause/suspend/stop/close thresholds and escalates through the stages the provider supports.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `environments`
- Role: `operator`
- Target: `none`
- Params: `EnvironmentIdlePolicyPutParams`
- Result: `AgentApiOutcome<EnvironmentIdlePolicyPutResponse>`

### `environments/provider-bindings/list`

**List environment provider bindings**

Lists this universe's revisioned routing and admission bindings to deployment-scoped physical providers.

- Access: `{"kind":"universe","action":"read"}`
- Group: `environments`
- Role: `viewer`
- Target: `none`
- Params: `EnvironmentProviderBindingListParams`
- Result: `AgentApiOutcome<EnvironmentProviderBindingListResponse>`

### `environments/provider-bindings/read`

**Read an environment provider binding**

Returns one universe routing and admission binding. Provider-wide templates and physical resource, network, and ingress policy remain provider-owned.

- Access: `{"kind":"universe","action":"read"}`
- Group: `environments`
- Role: `viewer`
- Target: `none`
- Params: `EnvironmentProviderBindingReadParams`
- Result: `AgentApiOutcome<EnvironmentProviderBindingReadResponse>`

### `environments/templates/list`

**List environment templates**

Reads immutable templates directly from the selected bound provider controller.

- Access: `{"kind":"universe","action":"read"}`
- Group: `environments`
- Role: `viewer`
- Target: `none`
- Params: `EnvironmentTemplateListParams`
- Result: `AgentApiOutcome<EnvironmentTemplateListResponse>`

### `environments/templates/read`

**Read an environment template**

Returns one immutable template version from the selected bound provider controller.

- Access: `{"kind":"universe","action":"read"}`
- Group: `environments`
- Role: `viewer`
- Target: `none`
- Params: `EnvironmentTemplateReadParams`
- Result: `AgentApiOutcome<EnvironmentTemplateReadResponse>`

### `environments/jobs/create`

**Create environment jobs**

Starts a dependency-aware job group on one environment instance, injecting the environment's configured credentials at provider start. requestId is the retry identity; jobs are owned by the instance rather than a session. A powered-down environment is woken on use: the call fails with environment_not_ready while the wake is in progress; retry with backoff.

- Access: `{"kind":"universe","action":"use_resource"}`
- Group: `environments`
- Role: `contributor`
- Target: `none`
- Params: `EnvironmentJobCreateParams`
- Result: `AgentApiOutcome<EnvironmentJobCreateResponse>`

### `environments/jobs/read`

**Read environment jobs**

Reads selected job handles with bounded output, optional sequence continuation, and optional artifacts; use returned status/sequence data for polling.

- Access: `{"kind":"universe","action":"read"}`
- Group: `environments`
- Role: `viewer`
- Target: `none`
- Params: `EnvironmentJobReadParams`
- Result: `AgentApiOutcome<EnvironmentJobReadResponse>`

### `environments/jobs/cancel`

**Cancel environment jobs**

Requests cancellation for selected jobs, optionally including dependents. Force is provider-specific escalation; inspect each per-job result.

- Access: `{"kind":"universe","action":"use_resource"}`
- Group: `environments`
- Role: `contributor`
- Target: `none`
- Params: `EnvironmentJobCancelParams`
- Result: `AgentApiOutcome<EnvironmentJobCancelResponse>`

### `environments/registration-keys/create`

**Mint an environment registration key**

Creates a reusable universe-scoped key that lets outbound envd daemons register as environments. The plaintext secret is returned exactly once; only its hash is stored. Identity mode, active limit, disconnect grace, and expiry are the key's policy. Treat the secret like a cluster-join credential.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `environments`
- Role: `operator`
- Target: `none`
- Params: `EnvironmentRegistrationKeyCreateParams`
- Result: `AgentApiOutcome<EnvironmentRegistrationKeyCreateResponse>`

### `environments/registration-keys/read`

**Read an environment registration key**

Returns the key's display prefix, policy, status, and derived environment counts; never the secret or its hash.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `environments`
- Role: `operator`
- Target: `none`
- Params: `EnvironmentRegistrationKeyReadParams`
- Result: `AgentApiOutcome<EnvironmentRegistrationKeyReadResponse>`

### `environments/registration-keys/list`

**List environment registration keys**

Lists this universe's registration keys with policy, status, and derived counts. Each key is the group of the environments it admitted.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `environments`
- Role: `operator`
- Target: `none`
- Params: `EnvironmentRegistrationKeyListParams`
- Result: `AgentApiOutcome<EnvironmentRegistrationKeyListResponse>`

### `environments/registration-keys/revoke`

**Revoke an environment registration key**

Stops the key from admitting new daemon identities; already registered daemons keep reconnecting. With closeEnvironments, also closes every non-closed environment the key admitted. Idempotent.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `environments`
- Role: `operator`
- Target: `none`
- Params: `EnvironmentRegistrationKeyRevokeParams`
- Result: `AgentApiOutcome<EnvironmentRegistrationKeyRevokeResponse>`

### `models/list`

**Discover available models**

Queries supported providers directly, with a brief process-local burst cache, and returns best-effort selectable routes. One provider failure does not discard successful results from others.

- Access: `{"kind":"universe","action":"read"}`
- Group: `models`
- Role: `viewer`
- Target: `none`
- Params: `ModelListParams`
- Result: `AgentApiOutcome<ModelListResponse>`

### `profiles/create`

**Create an agent profile**

Creates a new universe-scoped reusable profile document; use profiles/put for create-or-replace revision semantics.

- Access: `{"kind":"universe","action":"create_profile"}`
- Group: `profiles`
- Role: `operator`
- Target: `none`
- Params: `ProfileCreateParams`
- Result: `AgentApiOutcome<ProfileCreateResponse>`

### `profiles/read`

**Read an agent profile**

Returns the complete profile document and current revision.

- Access: `{"kind":"universe","action":"read"}`
- Group: `profiles`
- Role: `viewer`
- Target: `none`
- Params: `ProfileReadParams`
- Result: `AgentApiOutcome<ProfileReadResponse>`

### `profiles/list`

**List agent profiles**

Returns lightweight summaries of universe-scoped reusable profiles.

- Access: `{"kind":"universe","action":"read"}`
- Group: `profiles`
- Role: `viewer`
- Target: `none`
- Params: `ProfileListParams`
- Result: `AgentApiOutcome<ProfileListResponse>`

### `profiles/put`

**Create or replace an agent profile**

Stores the complete profile document. Use expectedRevision from profiles/read when replacing to prevent lost updates; absence writes unconditionally.

- Access: `{"kind":"universe","action":"manage_profile"}`
- Group: `profiles`
- Role: `operator`
- Target: `none`
- Params: `ProfilePutParams`
- Result: `AgentApiOutcome<ProfilePutResponse>`

### `profiles/delete`

**Delete an agent profile**

Deletes the catalog document; sessions previously created or configured from it retain their materialized state.

- Access: `{"kind":"universe","action":"manage_profile"}`
- Group: `profiles`
- Role: `operator`
- Target: `none`
- Params: `ProfileDeleteParams`
- Result: `AgentApiOutcome<ProfileDeleteResponse>`

### `blobs/put`

**Store content-addressed blobs**

Decodes and stores a batch of base64 payloads, returning immutable content references in request order. Re-uploading identical bytes is naturally deduplicated.

- Access: `{"kind":"universe","action":"use_resource"}`
- Group: `blobs/put`
- Role: `contributor`
- Target: `none`
- Params: `BlobPutParams`
- Result: `AgentApiOutcome<BlobPutResponse>`

### `blobs/read`

**Read a content-addressed blob**

Returns the complete immutable blob of this universe as base64; large values count against gateway and MCP response limits.

- Access: `{"kind":"universe","action":"read"}`
- Group: `session`
- Role: `viewer`
- Target: `none`
- Params: `BlobReadParams`
- Result: `AgentApiOutcome<BlobReadResponse>`

### `blobs/has`

**Check blob availability**

Checks a batch of content references without returning blob bodies, preserving request order.

- Access: `{"kind":"universe","action":"read"}`
- Group: `session`
- Role: `viewer`
- Target: `none`
- Params: `BlobHasParams`
- Result: `AgentApiOutcome<BlobHasResponse>`

### `vfs/snapshots/commit`

**Commit a VFS snapshot**

Validates and stores an immutable filesystem manifest. Upload referenced file blobs first; the returned snapshot ref is content-addressed.

- Access: `{"kind":"universe","action":"use_resource"}`
- Group: `vfs`
- Role: `contributor`
- Target: `none`
- Params: `VfsSnapshotCommitParams`
- Result: `AgentApiOutcome<VfsSnapshotCommitResponse>`

### `vfs/snapshots/read`

**Read a VFS snapshot**

Returns an immutable snapshot manifest and aggregate file/byte counts; file bodies remain separate blobs.

- Access: `{"kind":"universe","action":"read"}`
- Group: `vfs`
- Role: `viewer`
- Target: `none`
- Params: `VfsSnapshotReadParams`
- Result: `AgentApiOutcome<VfsSnapshotReadResponse>`

### `vfs/workspaces/create`

**Create a mutable VFS workspace**

Creates a universe workspace attributed to the caller at an optional seed snapshot; absence starts from a server-created empty snapshot.

- Access: `{"kind":"universe","action":"create_workspace"}`
- Group: `vfs`
- Role: `contributor`
- Target: `none`
- Params: `VfsWorkspaceCreateParams`
- Result: `AgentApiOutcome<VfsWorkspaceCreateResponse>`

### `vfs/workspaces/read`

**Read a VFS workspace**

Returns workspace metadata, current head snapshot, and revision for safe updates.

- Access: `{"kind":"universe","action":"read"}`
- Group: `vfs`
- Role: `viewer`
- Target: `none`
- Params: `VfsWorkspaceReadParams`
- Result: `AgentApiOutcome<VfsWorkspaceReadResponse>`

### `vfs/workspaces/list`

**List VFS workspaces**

Lists the mutable universe workspaces with head snapshots, sizes, and revisions.

- Access: `{"kind":"universe","action":"read"}`
- Group: `vfs`
- Role: `viewer`
- Target: `none`
- Params: `VfsWorkspaceListParams`
- Result: `AgentApiOutcome<VfsWorkspaceListResponse>`

### `vfs/workspaces/update`

**Update a VFS workspace**

Moves the workspace head to an existing snapshot, which requires use of the workspace, and updates its display name, which requires configuring it. Pass expectedRevision from a read to prevent lost updates.

- Access: `{"kind":"universe","action":"use_resource"}`
- Group: `vfs`
- Role: `contributor`
- Target: `none`
- Params: `VfsWorkspaceUpdateParams`
- Result: `AgentApiOutcome<VfsWorkspaceUpdateResponse>`

### `vfs/workspaces/delete`

**Delete a VFS workspace**

Deletes the mutable workspace record; immutable snapshots and blobs remain content-addressed resources.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `vfs`
- Role: `operator`
- Target: `none`
- Params: `VfsWorkspaceDeleteParams`
- Result: `AgentApiOutcome<VfsWorkspaceDeleteResponse>`

### `mcp/servers/put`

**Create or replace an MCP server record**

Stores the complete catalog document with its optional auth-grant credential. A new server is attributed to the caller. Use expectedRevision when replacing; token material is never accepted or returned.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `mcp`
- Role: `operator`
- Target: `none`
- Params: `McpServerPutParams`
- Result: `AgentApiOutcome<McpServerPutResponse>`

### `mcp/servers/auth/discover`

**Discover MCP server authentication**

Looks for standards-based OAuth protected-resource metadata without creating a server, OAuth client, flow, or grant. An absent OAuth result is inconclusive and callers must allow manual auth selection.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `mcp`
- Role: `operator`
- Target: `none`
- Params: `McpServerAuthDiscoverParams`
- Result: `AgentApiOutcome<McpServerAuthDiscoverResponse>`

### `mcp/servers/tools/discover`

**Discover MCP server tools**

Connects directly to the configured MCP server with its current universe credential and returns one bounded live tools/list result. The inventory is never persisted or cached and no tool is invoked.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `mcp`
- Role: `operator`
- Target: `none`
- Params: `McpServerToolsDiscoverParams`
- Result: `AgentApiOutcome<McpServerToolsDiscoverResponse>`

### `mcp/servers/read`

**Read an MCP server record**

Returns one catalog document with defaults, auth policy, non-secret grant binding, status, and revision; no credential value is exposed.

- Access: `{"kind":"universe","action":"read"}`
- Group: `mcp`
- Role: `viewer`
- Target: `none`
- Params: `McpServerReadParams`
- Result: `AgentApiOutcome<McpServerReadResponse>`

### `mcp/servers/list`

**List MCP server records**

Lists the universe catalog entries, optionally filtered by lifecycle/configuration status.

- Access: `{"kind":"universe","action":"read"}`
- Group: `mcp`
- Role: `viewer`
- Target: `none`
- Params: `McpServerListParams`
- Result: `AgentApiOutcome<McpServerListResponse>`

### `mcp/servers/delete`

**Delete an MCP server record**

Deletes the catalog document. Existing session configs that reference it are not silently rewritten and may need explicit reconfiguration.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `mcp`
- Role: `operator`
- Target: `none`
- Params: `McpServerDeleteParams`
- Result: `AgentApiOutcome<McpServerDeleteResponse>`

### `auth/grants/import`

**Import a static bearer grant**

Accepts a plaintext token, encrypts it immediately, and returns only grant metadata/token-presence flags. Brokered is the default; retrievable exposure is immutable and permits service-only leases.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `auth`
- Role: `operator`
- Target: `none`
- Params: `AuthGrantImportParams`
- Result: `AgentApiOutcome<AuthGrantImportResponse>`


## Service methods

### `auth/grants/lease`

**Lease a retrievable authentication grant**

Service callers only. Resolves the current access token through the broker, records the lease, and returns it once. Cache only in memory until expiry minus margin (or at most five minutes without expiry), re-lease after target 401/403, and never persist or place the token in workflow payloads.

- Access: `{"kind":"service"}`
- Group: `auth/lease`
- Role: `none`
- Target: `none`
- Params: `AuthGrantLeaseParams`
- Result: `AgentApiOutcome<AuthGrantLeaseResponse>`


## Universe methods

### `auth/grants/read`

**Read authentication grant metadata**

Returns creator attribution, provider binding, scopes, audience, expiry, status, and token-presence flags; access and refresh token values are never returned.

- Access: `{"kind":"universe","action":"read"}`
- Group: `auth`
- Role: `viewer`
- Target: `none`
- Params: `AuthGrantReadParams`
- Result: `AgentApiOutcome<AuthGrantReadResponse>`

### `auth/grants/list`

**List authentication grants**

Lists non-secret grant metadata for the universe, optionally filtered by status.

- Access: `{"kind":"universe","action":"read"}`
- Group: `auth`
- Role: `viewer`
- Target: `none`
- Params: `AuthGrantListParams`
- Result: `AgentApiOutcome<AuthGrantListResponse>`

### `auth/grants/revoke`

**Revoke an authentication grant**

Marks the grant unusable by token consumers while retaining non-secret audit metadata.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `auth`
- Role: `operator`
- Target: `none`
- Params: `AuthGrantRevokeParams`
- Result: `AgentApiOutcome<AuthGrantRevokeResponse>`

### `auth/clients/create`

**Register an OAuth client**

Stores provider endpoints and client identity; an optional plaintext client secret is encrypted and represented thereafter only by hasClientSecret.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `auth`
- Role: `operator`
- Target: `none`
- Params: `AuthClientCreateParams`
- Result: `AgentApiOutcome<AuthClientCreateResponse>`

### `auth/clients/read`

**Read OAuth client metadata**

Returns endpoints, public client identity, defaults, and secret-presence state; the client secret is never returned.

- Access: `{"kind":"universe","action":"read"}`
- Group: `auth`
- Role: `viewer`
- Target: `none`
- Params: `AuthClientReadParams`
- Result: `AgentApiOutcome<AuthClientReadResponse>`

### `auth/clients/list`

**List OAuth clients**

Lists non-secret OAuth client registrations in the universe.

- Access: `{"kind":"universe","action":"read"}`
- Group: `auth`
- Role: `viewer`
- Target: `none`
- Params: `AuthClientListParams`
- Result: `AgentApiOutcome<AuthClientListResponse>`

### `auth/clients/delete`

**Delete an OAuth client**

Deletes the client registration and its stored client secret; grants already created from it remain separate records.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `auth`
- Role: `operator`
- Target: `none`
- Params: `AuthClientDeleteParams`
- Result: `AgentApiOutcome<AuthClientDeleteResponse>`

### `auth/flows/start`

**Start an OAuth authorization flow**

Creates a short-lived PKCE flow carrying the immutable grant exposure choice and returns a browser authorization URL containing one-time state. Treat the URL as sensitive and poll auth/flows/read for completion.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `auth`
- Role: `operator`
- Target: `none`
- Params: `AuthFlowStartParams`
- Result: `AgentApiOutcome<AuthFlowStartResponse>`

### `auth/flows/read`

**Read OAuth flow status**

Polls a flow's pending/completed/failed/expired state and returns the resulting grant id when authorization succeeds; no token value is exposed.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `auth`
- Role: `operator`
- Target: `none`
- Params: `AuthFlowStatusParams`
- Result: `AgentApiOutcome<AuthFlowStatusResponse>`

### `auth/providers/create`

**Register an authentication provider**

Creates a model or GitHub credential source. Plaintext API keys/private keys are encrypted on receipt and later represented only by configuration plus hasCredential.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `auth`
- Role: `operator`
- Target: `none`
- Params: `AuthProviderCreateParams`
- Result: `AgentApiOutcome<AuthProviderCreateResponse>`

### `auth/providers/read`

**Read authentication provider metadata**

Returns provider kind, non-secret configuration, credential-presence state, and status; stored credentials are never returned.

- Access: `{"kind":"universe","action":"read"}`
- Group: `auth`
- Role: `viewer`
- Target: `none`
- Params: `AuthProviderReadParams`
- Result: `AgentApiOutcome<AuthProviderReadResponse>`

### `auth/providers/list`

**List authentication providers**

Lists non-secret model/GitHub provider registrations for the universe.

- Access: `{"kind":"universe","action":"read"}`
- Group: `auth`
- Role: `viewer`
- Target: `none`
- Params: `AuthProviderListParams`
- Result: `AgentApiOutcome<AuthProviderListResponse>`

### `auth/providers/delete`

**Delete an authentication provider**

Deletes the provider registration and its directly stored credential; separately stored grants remain independent records.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `auth`
- Role: `operator`
- Target: `none`
- Params: `AuthProviderDeleteParams`
- Result: `AgentApiOutcome<AuthProviderDeleteResponse>`

### `auth/github/installations/list`

**List GitHub App installations**

Uses the registered GitHub App provider credential to query accessible installations and returns account/permission metadata without tokens.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `auth`
- Role: `operator`
- Target: `none`
- Params: `AuthGitHubInstallationListParams`
- Result: `AgentApiOutcome<AuthGitHubInstallationListResponse>`

### `auth/github/installations/grant`

**Grant access to a GitHub App installation**

Creates or refreshes a universe auth grant for one accessible installation. The installation token is brokered internally and never returned.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `auth`
- Role: `operator`
- Target: `none`
- Params: `AuthGitHubInstallationGrantParams`
- Result: `AgentApiOutcome<AuthGitHubInstallationGrantResponse>`

### `bots/create`

**Create a bot**

Creates the bot record, optionally with its triggers, and starts its controller. Fails if the bot id exists; a trigger failure rolls the bot back.

- Access: `{"kind":"universe","action":"create_bot"}`
- Group: `bots`
- Role: `operator`
- Target: `none`
- Params: `BotCreateParams`
- Result: `AgentApiOutcome<BotCreateResponse>`

### `bots/put`

**Create or replace a bot document**

Replaces the mutable configuration whole and signals the controller, which applies it at its next idle boundary. Pass expectedRevision when replacing; a closed bot accepts label-only edits.

- Access: `{"kind":"universe","action":"manage_bot"}`
- Group: `bots`
- Role: `operator`
- Target: `none`
- Params: `BotPutParams`
- Result: `AgentApiOutcome<BotPutResponse>`

### `bots/read`

**Read a bot**

Returns the bot record, its current revision, and lifecycle columns.

- Access: `{"kind":"universe","action":"read"}`
- Group: `bots`
- Role: `viewer`
- Target: `none`
- Params: `BotReadParams`
- Result: `AgentApiOutcome<BotReadResponse>`

### `bots/list`

**List bots**

Returns the roster: every bot with its trigger count, pending event count, and latest event, optionally narrowed by createdBy.

- Access: `{"kind":"universe","action":"read"}`
- Group: `bots`
- Role: `viewer`
- Target: `none`
- Params: `BotListParams`
- Result: `AgentApiOutcome<BotListResponse>`

### `bots/close`

**Close a bot**

Terminal and idempotent: disables every trigger, drops schedules, and tells the controller to archive pending events and force-close its sessions. Returns once signalled; follow bots/state/read for closing to closed.

- Access: `{"kind":"universe","action":"manage_bot"}`
- Group: `bots`
- Role: `operator`
- Target: `none`
- Params: `BotCloseParams`
- Result: `AgentApiOutcome<BotCloseResponse>`

### `bots/delete`

**Delete a bot**

Closes the bot if needed, waits for its controller to complete, deletes the sessions it closed, and removes the record so the bot id is free again.

- Access: `{"kind":"universe","action":"manage_bot"}`
- Group: `bots`
- Role: `operator`
- Target: `none`
- Params: `BotDeleteParams`
- Result: `AgentApiOutcome<BotDeleteResponse>`

### `bots/state/read`

**Read bot controller state**

Queries the controller workflow for its live snapshot (sessions, buffers, active and recent deliveries, budget) and lists sub-agent descendants. The controller is absent until the bot's first event.

- Access: `{"kind":"universe","action":"read"}`
- Group: `bots`
- Role: `viewer`
- Target: `none`
- Params: `BotStateReadParams`
- Result: `AgentApiOutcome<BotStateReadResponse>`

### `bots/sessions/rotate`

**Rotate a bot session**

Asks the controller to close one of the bot's sessions at its next idle boundary and continue on a fresh generation; queued deliveries follow.

- Access: `{"kind":"universe","action":"manage_bot"}`
- Group: `bots`
- Role: `operator`
- Target: `none`
- Params: `BotSessionRotateParams`
- Result: `AgentApiOutcome<BotSessionRotateResponse>`

### `bots/triggers/put`

**Create or replace a trigger**

Validates the trigger document (CEL parses, grants exist, one inbox per bot, chat routes per conversation), reconciles its Temporal Schedule, and stores it. A poll spec edit resets the cursor; a webhook keeps its URL token.

- Access: `{"kind":"universe","action":"manage_bot"}`
- Group: `bots`
- Role: `operator`
- Target: `none`
- Params: `BotTriggerPutParams`
- Result: `AgentApiOutcome<BotTriggerPutResponse>`

### `bots/triggers/read`

**Read a trigger**

Returns one trigger with its incidents and cursor; the ingest path and pairing code are included for bot-management callers.

- Access: `{"kind":"universe","action":"read"}`
- Group: `bots`
- Role: `viewer`
- Target: `none`
- Params: `BotTriggerReadParams`
- Result: `AgentApiOutcome<BotTriggerReadResponse>`

### `bots/triggers/list`

**List a bot's triggers**

Returns every trigger of the bot ordered by id, secrets included for bot-management callers.

- Access: `{"kind":"universe","action":"read"}`
- Group: `bots`
- Role: `viewer`
- Target: `none`
- Params: `BotTriggerListParams`
- Result: `AgentApiOutcome<BotTriggerListResponse>`

### `bots/triggers/delete`

**Delete a trigger**

Drops the trigger's Temporal Schedule and pairings, then the record; stored events keep their history.

- Access: `{"kind":"universe","action":"manage_bot"}`
- Group: `bots`
- Role: `operator`
- Target: `none`
- Params: `BotTriggerDeleteParams`
- Result: `AgentApiOutcome<BotTriggerDeleteResponse>`

### `bots/events/admit`

**Admit an event manually**

Stores an operator-authored event for the bot's main session and wakes the controller. eventId is the dedupe identity; a duplicate returns the stored row.

- Access: `{"kind":"universe","action":"invoke_bot"}`
- Group: `bots`
- Role: `contributor`
- Target: `none`
- Params: `BotEventAdmitParams`
- Result: `AgentApiOutcome<BotEventAdmitResponse>`

### `bots/events/replay`

**Replay a stored event**

Re-admits the stored envelope as a fresh event with the original routing; the replay never coalesces.

- Access: `{"kind":"universe","action":"manage_bot"}`
- Group: `bots`
- Role: `operator`
- Target: `none`
- Params: `BotEventReplayParams`
- Result: `AgentApiOutcome<BotEventReplayResponse>`

### `bots/events/list`

**List a bot's events**

Cursor-paginated event log, newest first, with outcomes; payload documents stay in the CAS.

- Access: `{"kind":"universe","action":"read"}`
- Group: `bots`
- Role: `viewer`
- Target: `none`
- Params: `BotEventListParams`
- Result: `AgentApiOutcome<BotEventListResponse>`

### `bots/events/read`

**Read an event by number**

Returns the event row and its full stored envelope document.

- Access: `{"kind":"universe","action":"read"}`
- Group: `bots`
- Role: `viewer`
- Target: `none`
- Params: `BotEventReadParams`
- Result: `AgentApiOutcome<BotEventReadResponse>`

### `bots/filters/test`

**Test a CEL filter**

Evaluates a filter against one payload or a sample of recent stored events, reporting matches and evaluation errors without changing anything.

- Access: `{"kind":"universe","action":"manage_bot"}`
- Group: `bots`
- Role: `operator`
- Target: `none`
- Params: `BotFilterTestParams`
- Result: `AgentApiOutcome<BotFilterTestResponse>`

### `channels/accounts/create`

**Create a channel account**

Registers a provider account (Telegram, WhatsApp) for this universe. The credential is a retrievable grant reference; no token is accepted here.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `channels`
- Role: `operator`
- Target: `none`
- Params: `ChannelAccountCreateParams`
- Result: `AgentApiOutcome<ChannelAccountCreateResponse>`

### `channels/accounts/put`

**Create or replace a channel account**

Replaces the account document whole; pass expectedRevision when replacing. The connector host picks the change up on its next discovery pass.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `channels`
- Role: `operator`
- Target: `none`
- Params: `ChannelAccountPutParams`
- Result: `AgentApiOutcome<ChannelAccountPutResponse>`

### `channels/accounts/read`

**Read a channel account**

Returns the account document and revision.

- Access: `{"kind":"universe","action":"read"}`
- Group: `channels`
- Role: `viewer`
- Target: `none`
- Params: `ChannelAccountReadParams`
- Result: `AgentApiOutcome<ChannelAccountReadResponse>`

### `channels/accounts/list`

**List channel accounts**

Lists this universe's provider accounts, optionally by provider.

- Access: `{"kind":"universe","action":"read"}`
- Group: `channels`
- Role: `viewer`
- Target: `none`
- Params: `ChannelAccountListParams`
- Result: `AgentApiOutcome<ChannelAccountListResponse>`

### `channels/accounts/delete`

**Delete a channel account**

Removes the account and its pairings; chat triggers that reference it stop serving conversations.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `channels`
- Role: `operator`
- Target: `none`
- Params: `ChannelAccountDeleteParams`
- Result: `AgentApiOutcome<ChannelAccountDeleteResponse>`


## Service methods

### `channels/inbound/admit`

**Admit a provider message**

Service callers only. Resolves the chat trigger for the conversation, applies pairing, and signals the conversation workflow. Returns the decision so the connector can send pairing prompts itself; acknowledge the provider only after this returns.

- Access: `{"kind":"service"}`
- Group: `channels/inbound`
- Role: `none`
- Target: `none`
- Params: `ChannelInboundAdmitParams`
- Result: `AgentApiOutcome<ChannelInboundAdmitResponse>`


## Universe methods

### `channels/pairings/list`

**List chat pairings**

Lists conversations paired to chat triggers, optionally by account or bot.

- Access: `{"kind":"universe","action":"read"}`
- Group: `channels`
- Role: `viewer`
- Target: `none`
- Params: `ChannelPairingListParams`
- Result: `AgentApiOutcome<ChannelPairingListResponse>`

### `channels/pairings/delete`

**Unpair a conversation**

Removes one pairing; the conversation must present the pairing code again to reconnect.

- Access: `{"kind":"universe","action":"configure_resource"}`
- Group: `channels`
- Role: `operator`
- Target: `none`
- Params: `ChannelPairingDeleteParams`
- Result: `AgentApiOutcome<ChannelPairingDeleteResponse>`

### `channels/conversations/read`

**Read a conversation snapshot**

Queries the conversation workflow's live state for one chat, for debugging; absent when no workflow exists yet.

- Access: `{"kind":"universe","action":"read"}`
- Group: `channels`
- Role: `viewer`
- Target: `none`
- Params: `ChannelConversationReadParams`
- Result: `AgentApiOutcome<ChannelConversationReadResponse>`


## Deployment methods

### `deployment/environment-provider-bindings/list`

**List a universe's deployment provider bindings**

Deployment configuration inventory of one universe's provider bindings.

- Access: `{"kind":"deployment"}`
- Group: `deployment/environment-providers`
- Role: `none`
- Target: `none`
- Params: `DeploymentUniverseReadParams`
- Result: `AgentApiOutcome<EnvironmentProviderBindingListResponse>`

### `deployment/universes/create`

**Create a universe**

Creates the deployment tenant boundary for an explicit UUID. The operation is idempotent and reports whether a new universe was created.

- Access: `{"kind":"deployment"}`
- Group: `deployment/universes`
- Role: `none`
- Target: `none`
- Params: `DeploymentUniverseCreateParams`
- Result: `AgentApiOutcome<DeploymentUniverseCreateResponse>`

### `deployment/universes/list`

**List universes**

Returns deployment-wide universe summaries with approximate live aggregate counts and last session activity.

- Access: `{"kind":"deployment"}`
- Group: `deployment/universes`
- Role: `none`
- Target: `none`
- Params: `DeploymentUniverseListParams`
- Result: `AgentApiOutcome<DeploymentUniverseListResponse>`

### `deployment/universes/read`

**Read a universe**

Returns one deployment tenant summary with aggregate session, workspace, profile, and blob usage.

- Access: `{"kind":"deployment"}`
- Group: `deployment/universes`
- Role: `none`
- Target: `none`
- Params: `DeploymentUniverseReadParams`
- Result: `AgentApiOutcome<DeploymentUniverseReadResponse>`

### `deployment/universes/delete`

**Purge a universe**

Permanently terminates live session workflows, deletes external blob objects, and cascades universe data. The purge is resumable/idempotent after partial failure.

- Access: `{"kind":"deployment"}`
- Group: `deployment/universes`
- Role: `none`
- Target: `none`
- Params: `DeploymentUniverseDeleteParams`
- Result: `AgentApiOutcome<DeploymentUniverseDeleteResponse>`

### `deployment/api-keys/create`

**Create a scoped API key**

Mints a key for a universe or the deployment with the method groups it may call and whether it may assert actors. The plaintext secret is returned exactly once and cannot be recovered; persist only the displayed prefix for identification. Keys are immutable: revoke and mint to change what one may do.

- Access: `{"kind":"deployment"}`
- Group: `deployment/api-keys`
- Role: `none`
- Target: `none`
- Params: `DeploymentApiKeyCreateParams`
- Result: `AgentApiOutcome<DeploymentApiKeyCreateResponse>`

### `deployment/api-keys/list`

**List scoped API keys**

Returns non-secret key metadata, all keys or those of one scope, including groups, revocation and last-use timestamps. Plaintext secrets are never stored or returned.

- Access: `{"kind":"deployment"}`
- Group: `deployment/api-keys`
- Role: `none`
- Target: `none`
- Params: `DeploymentApiKeyListParams`
- Result: `AgentApiOutcome<DeploymentApiKeyListResponse>`

### `deployment/api-keys/revoke`

**Revoke a scoped API key**

Revokes the key with this display prefix; revoking a revoked key keeps its first revocation time. An unknown prefix is not found.

- Access: `{"kind":"deployment"}`
- Group: `deployment/api-keys`
- Role: `none`
- Target: `none`
- Params: `DeploymentApiKeyRevokeParams`
- Result: `AgentApiOutcome<DeploymentApiKeyRevokeResponse>`

### `deployment/environment-providers/put`

**Put an environment provider**

Registers or replaces one deployment provider and its controller connection. The provider does not call this API or require access to Lightspeed.

- Access: `{"kind":"deployment"}`
- Group: `deployment/environment-providers`
- Role: `none`
- Target: `none`
- Params: `DeploymentEnvironmentProviderPutParams`
- Result: `AgentApiOutcome<DeploymentEnvironmentProviderPutResponse>`

### `deployment/environment-providers/list`

**List environment providers**

Returns every deployment-registered deployment provider and its controller connection.

- Access: `{"kind":"deployment"}`
- Group: `deployment/environment-providers`
- Role: `none`
- Target: `none`
- Params: `DeploymentEnvironmentProviderListParams`
- Result: `AgentApiOutcome<DeploymentEnvironmentProviderListResponse>`

### `deployment/environment-providers/read`

**Read an environment provider**

Returns one deployment-registered deployment provider and its controller connection.

- Access: `{"kind":"deployment"}`
- Group: `deployment/environment-providers`
- Role: `none`
- Target: `none`
- Params: `DeploymentEnvironmentProviderReadParams`
- Result: `AgentApiOutcome<DeploymentEnvironmentProviderReadResponse>`

### `deployment/environment-providers/delete`

**Delete an environment provider**

Deletes a deployment provider only when no universe binding references it.

- Access: `{"kind":"deployment"}`
- Group: `deployment/environment-providers`
- Role: `none`
- Target: `none`
- Params: `DeploymentEnvironmentProviderDeleteParams`
- Result: `AgentApiOutcome<DeploymentEnvironmentProviderDeleteResponse>`

### `deployment/environment-providers/bindings/put`

**Put an environment provider binding**

Creates or replaces one universe's complete revisioned routing and admission binding. A deployment provider may have at most one binding in a universe.

- Access: `{"kind":"deployment"}`
- Group: `deployment/environment-providers`
- Role: `none`
- Target: `none`
- Params: `DeploymentProviderBindingPutParams`
- Result: `AgentApiOutcome<DeploymentProviderBindingPutResponse>`

### `deployment/environment-providers/bindings/delete`

**Delete an environment provider binding**

Deletes a universe provider binding only after every referencing environment has reached Closed.

- Access: `{"kind":"deployment"}`
- Group: `deployment/environment-providers`
- Role: `none`
- Target: `none`
- Params: `DeploymentProviderBindingDeleteParams`
- Result: `AgentApiOutcome<DeploymentProviderBindingDeleteResponse>`

### `deployment/environments/adopt`

**Adopt a provider environment**

Creates a universe environment by transferring an existing provider target into Lightspeed's managed lifecycle. The caller must explicitly accept ownership transfer.

- Access: `{"kind":"deployment"}`
- Group: `deployment/environment-providers`
- Role: `none`
- Target: `none`
- Params: `DeploymentEnvironmentAdoptParams`
- Result: `AgentApiOutcome<DeploymentEnvironmentAdoptResponse>`

### `deployment/channels/accounts/list`

**List channel accounts across universes**

The connector host's discovery call: every enabled provider account of the deployment with its universe id and credential grant reference. Re-poll to pick up accounts created or disabled since.

- Access: `{"kind":"deployment"}`
- Group: `deployment/channels`
- Role: `none`
- Target: `none`
- Params: `DeploymentChannelAccountListParams`
- Result: `AgentApiOutcome<DeploymentChannelAccountListResponse>`

