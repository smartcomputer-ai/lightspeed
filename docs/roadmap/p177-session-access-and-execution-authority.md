# P177 — Session access and execution authority

**Status:** Proposed. Second slice of
[enterprise authorization and identity](later/pNNN-enterprise-authorization.md),
building on [identity foundation and universe authorization](p176-identity-and-universe-authorization.md).
Includes the hardening items left open by that slice's review where they fit.

Lightspeed is still greenfield. Migrations are edited in place, contracts are
reshaped where the model needs it, and there is no compatibility path for the
current universe-visible content model.

## Outcome

A person can keep an investigation private in a universe their team shares, share
it with named people or groups when they choose, and see who a session runs as.
Every run carries a recorded execution identity; when that identity loses its
authority, the run stops at its next turn. Administrators govern and stop work
without reading private content unless they hold an explicit, audited permission.

Today none of this exists: sessions have no audience (every Viewer reads every
session), a run records no identity ([`RunRecord`](../../crates/engine/src/core/components/run.rs)),
worker-side credential resolution reaches the broker with no principal
([`BrokerSecretResolver`](../../crates/temporal-server/src/worker/secrets.rs)),
and the only worker-side identity, `ControllerAuthority`, names a resource rather
than a principal.

## Decisions

These settle what the parent document leaves open.

1. **Revocation is per turn, not per effect.** A run's authority is checked at
   admission and at the start of every model call. The turn in progress was
   authorized, so its tool batch, awaits and retries complete; the next model
   call fails the run with a typed `authority_revoked` failure. A long await or
   retry can defer that; accepted for now. Adapters do not check authority.
2. **Execution identity is immutable per session.** A session records who it runs
   as at creation and never changes it. Work under a different identity is a new
   session with deliberate context transfer.
3. **One audience per tree, and a bot is the root of its sessions.** History
   forks and delegated children share their root's policy, as they share its
   retention root; the sessions a bot creates, and their children, share the
   bot's policy. Config-only clones are independent roots.
4. **Policy is separate from ownership.** Ownership rows keep the immutable
   control facts (creator, controller, owner, managing bot, execution). A policy
   row holds the mutable visibility, `universe` or `restricted`, and a grant row
   holds one permission for one principal or group. All three share the resource
   key, so later slices restrict workspaces, environments and MCP servers with
   policy and grant rows and their own permission vocabularies, without an owner
   or an execution identity they do not have.
5. **Policy rows are explicit and fail closed.** A resource kind that the policy
   system covers gets its policy row atomically at creation, `universe` included;
   a missing row denies. Kinds not yet covered keep their role rules. This slice
   covers sessions and bots; profiles stay on role rules.
6. **Role rights no longer short-circuit.** Every resource decision reads the
   resource's ownership, its root's policy and the caller's grant in one
   statement, then decides. Only a role `Denied` returns without reading.
7. **Administrators do not read private content.** Operator/Admin keep stop
   rights and Admin gains delete on any session or bot, restricted ones included,
   with metadata only. Reading restricted content without a grant requires the
   explicit `read_private_content` capability, which no role implies and whose
   every use is audited.
8. **Personal execution is attribution and a revocation source, nothing more,
   until per-person credentials exist.** A personal run uses the same universe
   resources a service run would. What differs: the run is attributed to the
   person, the session is restricted by default with no writers, and disabling
   the person or removing their universe access stops the work. The owner may
   still add a writer explicitly; that writer's runs execute under the owner's
   authority with the writer as `authorizedBy`, so the delegation is attributed
   and ends with the membership. The UI does not offer it.
9. **A content hash is never access.** A blob is read through a named resource:
   the caller must be able to read that resource, and the blob must be validated
   content of it. Retention references and authorization are different facts:
   the collector keeps every exact reference it finds so nothing is swept, but
   only references the runtime itself wrote into content positions, or admitted
   through an authorized attachment, confer reads. Writing a hash into your own
   session does not make its bytes readable. The same rule governs attaching
   existing content, and a fresh upload is readable by its uploader.
10. **A bot is a thin wrapper around its sessions and shares their access
    model.** One policy row governs the bot, its events and activity, and every
    session it creates, as one audience; those sessions carry no policy of their
    own. `read` on a bot reads all of it; `write` also invokes the bot and
    controls its sessions. Configuring the bot stays with its owner and the
    ManageBot role. A universe-visible bot follows role rules; a personal bot is
    restricted to its owner until shared.
11. **Contributors keep universe-wide resource use.** Per-resource restriction is
    the next slice. This slice only closes the paths by which a Contributor reads
    or exfiltrates a stored secret (see hardening).

## Scope

### 0. Hardening

Small, independent of the model, done first.

- **Explicit auth mode.** `LIGHTSPEED_AUTH_MODE` is required
  ([config.rs](../../crates/temporal-server/src/config.rs) defaults to `single`);
  `./dev.sh` sets it explicitly.
- **Credentials only reach their audience.** A bot poll trigger may attach a
  grant only when the grant's `audience` matches the trigger URL's origin; a
  grant without an audience needs `ConfigureResource`. Poll URLs get the same
  private-network guard as outbound MCP. This also stops the Configurator key,
  imported as a grant with the MCP URL as audience, from being sent elsewhere.
- **Platform sign-in.** Better Auth account linking off (`accountLinking:
  { enabled: false }`); first-time external sign-up creates its core principal in
  the `before` hook instead of failing on the required `corePrincipalId` field; a
  live test signs up through the real path, not `internalAdapter.createUser`.
- **Managed sessions.** `session/managed/start` verifies that `receiver.workflowId`
  belongs to the caller's universe like every other workflow reference.
- **Directory scope.** `deployment/identity/directory` in universe scope returns
  only principals and groups holding a role in that universe; the deployment-wide
  listing stays with DeploymentAdmin and `manage_identity`. The share dialog below
  needs exactly the universe-scoped form.
- **Platform errors.** The gateway `onError` returns a generic body for unknown
  errors and logs the detail; database error text never reaches a client.
- **Enforcement test.** The reviewers' "every handler authorizes" script becomes a
  unit test over the method manifest.

Left as is, deliberately: DeploymentAdmin minting keys for other people (headless
onboarding needs it) and the Configurator's Operator key. The Configurator is a
universe resource any Contributor's session can attach; the correct fix is a
restricted resource policy or requester propagation, both next slice. Installing
it remains a universe Admin action and its setup says what it grants.

### 1. Session access policy

Records, all keyed by `(universe_id, resource_kind, resource_id)`:

| Record | Holds | Mutable |
| --- | --- | --- |
| `access_resource_ownership` | creator, controller, owner, managing bot, `audience_root` (kind + id), and for sessions and bots `run_as_principal_id` + `execution_kind` | no |
| `access_resource_policies` | `visibility` for a root resource; who changed it and when | yes |
| `access_resource_grants` | one `permission` for a principal or group on a root resource; who granted it and when | yes |

`audience_root` is copied at reservation like owner and bot: itself for a root
session or a bot, the parent's root for a fork or delegated child, the bot for a
session a bot creates. Policy and grant rows exist only for roots; everything
below resolves to its root's rows. Permission vocabularies are per resource kind
and validated in `access`; this slice defines `read` and `write` for sessions and
bots with the same meaning: `read` sees the tree, `write` controls it.

```text
Read            = Read allowed AND (root universe OR owner OR grant on root OR privileged)
Control session = owner OR write grant on root OR (managing bot AND ManageBot allowed)
Invoke bot      = InvokeBot allowed (universe bot) OR owner OR write grant
Manage bot      = owner, OR ManageBot allowed when the bot is universe-visible
Stop            = StopSession allowed (Operator/Admin) OR owner OR write grant
Delete / close  = owner OR Admin
```

- **One evaluator.** `authorize(caller, action, resource)` replaces
  `resource_permitted`/`resource_actions`: after the role decision, one statement
  loads the ownership row, the root's policy and the caller's best grant (through
  `access_group_members`), and `access` decides. `session/list` and the bot list
  apply the same predicate in SQL, so lists return readable resources only,
  including children listed by origin.
- **Sharing.** One method pair for every root kind: `access/policy/read
  { resource }` returns owner, visibility, run-as and grants;
  `access/policy/update { resource }` replaces visibility and the grant set.
  Write members share read access or make the resource universe-visible; only
  the owner adds writers or hands ownership to another member; readers change
  nothing. On a personal session or bot only the owner may add a writer, and the
  share dialog offers read only. A session a bot created has no policy of its
  own: reading its access shows the bot's, and sharing it means sharing the bot.
  Changes are audited (`audit: true`) and recorded as typed rows in
  `access_audit_changes` in the same transaction. Subjects must currently hold a
  role in the universe; losing universe access ends shared access without editing
  grants.
- **Content surfaces.** The policy applies to `session/read`, `session/list`,
  `session/events/read` and its long polls, run and approval reads, and the
  access preview. Control actions additionally require a write grant.
- **Blobs.** `blobs/read` and `blobs/has` take `resource: { kind, id }` (session,
  bot, workspace or profile) and succeed only when the caller may read that
  resource and the blob is validated content of it; a resource that does not
  authorize the read is a refusal, never a prompt to search for another. Without
  a resource, both succeed only for a blob the caller uploaded. Validated content
  is defined by origin, not reachability: `cas_session_roots` and
  `cas_bot_event_roots` gain an `origin` (`content` for references the engine's
  typed extraction found in content positions and for admitted attachments,
  `scan` for everything the generic collector adds). Reads follow `content` roots
  and the structured writers' `cas_blob_edges` within a depth and row budget;
  an exhausted budget rejects. A workspace's content is its head and base
  snapshot manifests and their files. Uploads: `blobs/put` records the uploader
  in `blob_uploads`, cascading with the blob; the sweeper's age cutoff already
  gives an upload its window before attachment. Attaching an existing reference
  anywhere (run input, context append, snapshot commit, workspace creation from a
  snapshot, profile documents, managed-session input) is accepted only for the
  caller's own upload, content of the target session's tree, or content of a
  named source the caller may read. The typed extraction is an engine function;
  a format test keeps it a subset of the generic collector and flags new
  reference-bearing fields.
- **Bots.** Bot creation writes the bot's policy row; a personal bot is
  `restricted`. Bot events, activity, and every session the bot creates are read
  through that row; `write` invokes the bot and controls its sessions; trigger
  configuration and secrets stay with the owner and ManageBot as today. Sessions
  a bot creates carry the bot as audience root and no policy row. The Access
  panel is one component for bots and sessions.
- **Workspaces and environments stay universe resources.** Files a restricted
  session writes into a shared workspace are as visible as that workspace, and
  `vfs/snapshots/read` stays universe-scoped until workspaces get policy rows.
  The UI says so where a session attaches one.
- **Release on delete.** Deleting a session removes its ownership, policy and
  grant rows with the rest of the cascade, so its id is free again; the id
  namespace belongs to the universe, not to whoever used it first. Admin may
  delete any session, which also covers offboarded owners.
- **Web.** An Access panel (owner, running as, visibility, members) and a share
  dialog fed by the universe-scoped directory; restricted sessions and bots are
  marked in lists; a bot's session shows its access as the bot's; the permission
  preview gains `read` and `share` decisions.

### 2. Execution identity

- **One execution service per universe.** Universe creation also creates a
  service principal managed in that universe, assigns it Contributor, and stores
  it on the universe row as `execution_principal_id`. Universe deletion disables
  it; attribution survives. Deployments created before this slice are reset, as
  the migration baseline already requires.
- **Universe execution policy.** `personal_execution_enabled` on the universe
  row, off by default. `access/execution/read` and `access/execution/update`
  (Admin, `ManageAccess`, audited).
- **Resolution at creation.** `SessionStartParams.execution` requests
  `{ kind: service | personal }`; a profile may request the same and confers
  nothing. `service` runs as the universe execution service. `personal` requires
  personal execution to be enabled, a user principal, and that the run-as is the
  caller; the session then defaults to `restricted`. A request that is not
  allowed is refused with `forbidden`; there is no fallback. Children copy the
  parent's execution; bot sessions copy the bot's; a bot's execution is chosen at
  `bots/create` under the same rules, with the bot owner as the personal run-as.
  Execution stays on the ownership row because it is reserved in the same
  transaction and copied to children by the same mechanism; a named-binding
  reference can join it there in a later slice.
- **Visible.** Session and bot summaries and `access/policy/read` expose owner,
  visibility and run-as; the web UI shows "Running as" on a session and offers
  the personal choice only where the universe enables it.

### 3. Run admission and turn-boundary authority

- **Admission.** Every run start — API, bot trigger, sub-agent, workflow tool —
  passes `admit_run`, which resolves the session's run-as principal and requires
  it active with `UseResource` in the universe. The requester's own control check
  is unchanged and separate. Admission records
  `RunAuthority { runAs, authorizedBy, parent }` on the accepted-run event, so the
  run record and replay carry who ran and who asked. `authorizedBy` is the acting
  principal for API starts, `Internal { component, cause }` for bot triggers, and
  the parent run for delegated children. Scalars only; the engine reads no policy.
- **Turn check.** `llm_generate` begins by resolving the run-as principal's
  current rights (one statement; two for a personal run, whose owner must also
  hold universe access). A model call takes seconds, so there is no revision
  shortcut. A failed check returns a typed, non-retryable activity failure and the
  workflow fails the run with `RunFailureKind::AuthorityRevoked`, leaving the
  session open. Queued runs meet the same check at their first model call.
  Effects already in flight finish with their turn; environment processes that
  outlive a turn are leftovers for the environment, as today.
- **One caller type.** `ControllerAuthority` becomes the run form of a caller:
  shared services authorize `Request(RequestContext)` and `Run(RunAuthority)`
  through the same path, a run acting as its run-as principal for its own
  resource. Bot workers and sub-agent delegation stop carrying a resource-only
  authority; the bot preamble and channel reads use the bot's run authority.
- **Stopping standing bot work.** Disabling a personal bot's owner, removing
  their universe access, or disabling the universe execution service fails each
  subsequent admission; the bot records the refusal in its activity. Closing the
  bot cancels its in-flight work as today. Automatic pausing after repeated
  refusals is not in this slice.

### 4. Private-content access

- A universe-scoped `read_private_content` capability, assignable to a user
  principal through the existing capability changes (Admin, audited, never
  implied by a role). Holders read restricted sessions and bots they hold no
  grant on and may take ownership of a restricted session.
- Each such read writes an `access_audit_events` row with `privileged = true`
  even though reads are otherwise quiet: the handler marks the request context
  when a decision relied on the capability and the boundary writes the row.
- API and CLI only; the web UI shows the capability in role editing and marks
  privileged reads, nothing more.

## Contract sketch

Illustrative shapes, not final wire signatures:

```text
ResourceOwnership = created_by + controller + owner + bot + audience_root(kind, id)
                  + execution(run_as, service | personal)   [sessions, bots]
ResourcePolicy    = visibility(universe | restricted)
Grant             = subject(principal | group) + permission + granted_by
ResourceAccess    = ownership + root policy + caller's best grant (one statement)

Caller            = Request(RequestContext) | Run(RunAuthority)
RunAuthority      = run_as + authorized_by(Principal | Internal | ParentRun) + parent
UniverseExecutionPolicy = execution_principal + personal_execution_enabled

authorize(caller, action, resource)      -> allowed | forbidden | not_found (+ privileged marker)
admit_run(caller, session)               -> RunAuthority | forbidden
check_turn(RunAuthority)                 -> ok | authority_revoked
may_read_blob(caller, resource, blob)    = authorize(caller, Read, resource)
                                           AND validated_content(resource, blob)
```

`access/policy/read|update`, `access/execution/read|update`, `execution` on
session and bot creation, `access` on session and bot summaries, `read`/`share`
in the access preview, `resource` on `blobs/read` and `blobs/has`, source naming
on attachments, `privileged` on audit events, and `authority_revoked` as a run
failure kind are the contract additions. Method `access:` and `audit:`
declarations stay mandatory.

## Persistence

Edited in place; schema revision advances once and the release metadata with it.

- `010_resource_ownership.sql`: `audience_root_kind`, `audience_root_id`,
  `run_as_principal_id` and `execution_kind` on `access_resource_ownership`, the
  latter two constrained to sessions and bots; `access_resource_policies (key,
  visibility, updated_by, updated_at_ms)`; `access_resource_grants (key,
  subject_kind, subject_id, permission, granted_by, granted_at_ms)` with an index
  on the subject. Both new tables reference the root's ownership row and cascade
  with it.
- `001_core.sql`: `execution_principal_id` and `personal_execution_enabled` on
  `universes` (or a follow-on file, if the principal foreign key needs
  `007_identity_access.sql` first); `origin` on `cas_session_roots`;
  `blob_uploads (universe_id, digest, principal_id, uploaded_at_ms)` cascading
  with `cas_blobs`.
- `008_bots.sql`: `origin` on `cas_bot_event_roots`.
- `011_access_audit.sql`: `privileged boolean NOT NULL DEFAULT false` on
  `access_audit_events`; new typed change rows for sharing and execution policy.
- Engine: `authority` on the accepted-run event and run record;
  `AuthorityRevoked` failure kind; typed content-reference extraction beside
  `collect_blob_refs`.

## Implementation order

Each step ships on its own; the order is by dependency.

1. [ ] Hardening (scope 0), including the manifest enforcement test.
2. [ ] Ownership additions, policy and grant rows written at reservation for
       root sessions and bots, the one-statement `authorize` with unit coverage
       in `access`; session and bot lists and content surfaces enforce the
       policy; release on delete; Admin delete. Live matrix through HTTP: owner,
       reader, writer, group member, Viewer, Operator, Admin against read, list,
       events, control, stop, delete and share, for a root session, a shared bot
       and a restricted bot with the sessions and events under them.
3. [ ] Content: typed extraction and root origins, `blob_uploads`, `resource` on
       `blobs/read` and `blobs/has`, attachment authorization on every path that
       accepts an existing reference. Live: a private blob's exact reference
       placed in the attacker's own session through input text, a scripted tool
       argument and a webhook payload stays unreadable through that session; an
       upload is readable by its uploader only; attaching a foreign reference is
       refused; an exhausted traversal budget rejects.
4. [ ] Universe execution service, execution policy methods, execution
       resolution at session/bot creation, "Running as" in summaries and web.
5. [ ] `admit_run`, `RunAuthority` on the run record, the `llm_generate` turn
       check, `AuthorityRevoked`, the unified caller type, and bot/sub-agent
       migration off `ControllerAuthority`. Live: disable an owner mid-run and
       require the current turn to complete and the next to fail with the kind;
       a sub-agent child failing after its parent's principal is disabled; a
       personal bot refusing admission after its owner is disabled.
6. [ ] `read_private_content`, privileged reads and their audit rows, ownership
       takeover.
7. [ ] Web: Access panel, share dialog, restricted markers, execution choice,
       workspace visibility note; Platform mapping of the new methods; docs
       (`multi-tenancy.md`, `authentication-and-tenancy.md`, API reference).

## Validation

- Unit: access decisions for every visibility × grant × role combination, with a
  missing policy row denying; only the owner adds a writer to a personal
  session, and that writer's run records the owner as run-as and the writer as
  authorizer; execution resolution refuses without fallback; typed extraction is
  a subset of the generic collector on every event fixture; audit target
  extraction for the new methods.
- Live (disposable services, serialized): the HTTP access matrix above; the
  content cases in step 3; execution recorded on run records and visible in
  summaries; turn-boundary revocation for API, bot and sub-agent runs;
  privileged reads writing exactly one row each; hardening cases (audience
  mismatch, private network, missing auth mode, real external sign-up,
  directory scoping, error body).
- Measured: an ordinary read stays at one key read, one rights read and one
  access lookup; a blob read adds one containment query; a model call adds one
  rights read.

## Boundary and follow-up

Not in this slice: named execution bindings with resource scopes, policy rows
and permission vocabularies for workspaces, environments, MCP servers and
grants (next slice, on the same records; `vfs/snapshots/read` then takes a
workspace context), requester propagation to first-party MCP servers such as
the Configurator, SSO and
provisioning, personal event-driven automation, source-imposed audience limits,
audit retention and export, a two-person rule or UI flow for privileged access,
and effect-time checks inside adapters.

After this slice the parent sequence continues with resource restrictions and
delegation enforcement, then SSO and provisioning, whose offboarding must govern
API keys and the standing bot authority defined here.

Current seams: [authorization service](../../crates/temporal-server/src/gateway/service/authorization.rs),
[ownership store](../../crates/store-pg/src/ownership.rs),
[reference collector](../../crates/engine/src/storage/blobs.rs),
[model-call activity](../../crates/temporal-server/src/worker/activities/llm.rs),
[bot worker](../../crates/temporal-server/src/worker/bots.rs),
[Platform gateway](../../platform/server/src/routes/gateway.ts).
