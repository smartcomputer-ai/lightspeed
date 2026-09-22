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
A collection gives sessions and bots one audience and one execution identity
that administrators govern in one place. Every run carries a recorded execution
identity; when that identity loses its authority, the run stops at its next turn.
Administrators govern and stop work without reading private content unless they
hold an explicit, audited permission.

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
2. **Execution identity is immutable per session, and personal execution binds
   the owner.** A session records who it runs as at creation and never changes
   it. Ownership of a personal session or collection never changes hands,
   because its owner is the authority it runs under. Work under a different
   identity is a new session with deliberate context transfer: a fork.
3. **Audiences belong to roots, and there are two root kinds.** A root is a
   standalone session or a collection. Delegated children share their parent's
   root; a bot, its events, and every session it creates share the bot's
   collection. A history fork or a config-only clone is a new root owned by
   whoever made it, holding content the maker could already read; that is the
   authorized context transfer the parent requires.
4. **One anchor per governed resource; the mutable governance lives once, on
   the root.** Every governed resource has an anchor row keyed
   `(universe, kind, id)` holding only immutable facts: creator, admitted
   controller, audience root, and, for sessions, bots and collections, the
   managing bot and execution. A root's policy row holds what can change: its
   current owner and visibility; grant rows hold one permission for one
   principal or group. Resources below a root have no owner of their own, so
   handing a root to someone else moves the whole tree in one row and the
   previous owner keeps nothing. Owner-less resources such as workspaces and
   MCP servers get an anchor and a policy without execution later.
5. **Policy rows are explicit and fail closed.** A resource kind that the policy
   system covers gets its policy row atomically at creation, `universe` included;
   a missing row denies. Kinds not yet covered keep their role rules. This slice
   covers sessions, bots and collections; profiles get a policy row too, so
   one rule holds for every anchored kind, but stay on role rules with no
   sharing surface.
6. **Role rights no longer short-circuit.** Every resource decision reads the
   resource's anchor, its root's policy and the caller's grant in one statement,
   then decides. Only a role `Denied` returns without reading.
7. **Administrators do not read private content.** Operator/Admin keep stop
   rights and Admin gains delete on any session, bot or collection, restricted
   ones included, with metadata only. Reading restricted content without a grant
   requires the explicit `read_private_content` capability, which no role implies
   and whose every use is audited. The capability reads; it never controls or
   takes ownership.
8. **Personal execution is attribution and a revocation source, nothing more,
   until per-person credentials exist.** A personal run uses the same universe
   resources a service run would. What differs: the run is attributed to the
   person, the root is restricted by default with no writers, and disabling the
   person or removing their universe access stops the work. The owner may still
   add a writer explicitly; that writer's runs execute under the owner's
   authority with the writer as `authorizedBy`, so the delegation is attributed
   and ends with the membership. The UI does not offer it.
9. **A content hash is never access, and authorization never traverses.** A
   blob is read through a named resource: the caller must be able to read that
   resource and the blob must be admitted content of it. Admission writes the
   authorization: the engine's typed extraction records the references the
   runtime placed in content positions, and a structured writer that admits a
   manifest or wrapper records its children in the same transaction, having
   verified each one. Uploading bytes authorizes exactly those bytes for the
   uploader; a nested reference needs its own admission. Retention keeps
   scanning every exact reference so nothing is swept, but scanned references
   confer nothing, and edges are never followed to decide a read.
10. **A bot is a thin wrapper around its sessions, and may be a member of a
    collection.** A bot's sessions always carry the bot's audience: created
    into a collection its creator may write to, the bot is a member and the
    collection's policy governs the bot, its events and activity, and every
    session it creates, none of which carries a policy of its own; created
    on its own, the bot is its own root with the same effect. Nothing
    creates a collection implicitly. `read` on the
    collection sees all of it; `write` also invokes its bots and controls its
    sessions. What stays the bot's own is control, not audience: routing,
    admission, its worker's authority over its sessions, and configuration,
    which belongs to the owner and the ManageBot role. A universe-visible
    collection follows role rules, so Contributors invoke its bots without
    controlling their sessions; a personal bot's collection is restricted to
    its owner until shared. Invoke-only grants and conversations private to
    their invoker are deliberately not supported here.
11. **Contributors keep universe-wide resource use.** Per-resource restriction is
    the next slice. This slice only closes the paths by which a Contributor reads
    or exfiltrates a stored secret (see hardening).
12. **Four relationships, kept independent.** A tool binding says where an
    operation goes and how it completes; a lifecycle controller says which
    workflow coordinates a session; a policy says who reads or controls it; an
    execution identity says which principal supports the work. The anchor holds
    them as separate facts (`controller`, `audience_root`, execution; bindings
    are session declarations), and a workflow may take part in any combination.
    The audience of a new session is a policy choice made by whoever holds the
    authority to make it: the requester (a principal, or a service asserting
    one) sets `access` on a standalone session it will own; a run's delegated
    child takes the parent's root; a caller with write on a collection may
    create in it and inherit. Inheritance is one choice, never the rule for
    every controller, and a controller's privilege over the protocol never
    decides the audience. Reservation already refuses a creator without an
    anchor of its own, so no session exists without one of the three.
13. **Plugins are principals; the controller context is the runtime's own.**
    A workflow plugin — a tool receiver, a lifecycle controller, a standing
    automation — authenticates with a service identity and is authorized like
    any principal: what it owns, what it is granted, and the invocations
    delivered to it. Components the runtime itself hosts (bots, sub-agent
    delegation, channel workflows) act through a controller context: universe,
    acting resource, execution principal, cause. Reads resolve through the
    actor's audience root; control comes from the actor alone: itself, a bot's
    own sessions, and the children a run admitted. Sharing an audience never
    confers control over its other members, so a session in a collection
    cannot steer a sibling and a delegated child cannot control its root.
    Resource use is the execution principal's rights, nothing else, so two
    sessions that run as the same universe execution service cannot read each
    other. Run provenance is the admission's audit row and the session's
    immutable execution identity; nothing is recorded on the run itself, and
    a run record is never a caller.

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

### 1. Access policy

Records, all keyed by `(universe_id, resource_kind, resource_id)`:

| Record | Holds | Mutable |
| --- | --- | --- |
| `access_resources` (the anchor; today's ownership table renamed) | creator, admitted controller, `audience_root` (kind + id), created_at; for sessions, bots and collections the managing bot, `run_as_principal_id` and `execution_kind` | no |
| `access_resource_policies` | a root's current owner and `visibility`; who changed them and when | yes |
| `access_resource_grants` | one `permission` for a principal or group on a root; who granted it and when | yes |
| `collections` | id and display name; everything else is on the anchor and policy | name |

`audience_root` is copied at reservation like the managing bot: itself for a
standalone session or a collection; the parent's root for a delegated child;
the collection for a bot and for every session created in it. Policy and grant
rows exist only for roots; everything below resolves to its root's rows, owner
included. Permission vocabularies are per resource kind and validated in
`access`; this slice defines `read` and `write` for sessions and collections
with one meaning: `read` sees the tree, `write` controls it.

```text
owner             = the root's current owner, for every resource in the tree
Read              = Read allowed AND (root universe OR owner OR grant on root OR privileged)
Control session   = owner OR write grant on root OR (managing bot AND ManageBot allowed)
Create in root    = owner OR write grant on root; a bot's worker, in its collection
Invoke bot        = InvokeBot allowed (universe collection) OR owner OR write grant
Manage bot        = owner, OR ManageBot allowed when the collection is universe-visible
Manage collection = owner, OR ConfigureResource allowed when universe-visible
Stop              = StopSession allowed (Operator/Admin) OR owner OR write grant
Delete / close    = owner OR Admin OR (managing bot AND ManageBot allowed)
Delete collection = owner OR Admin, once empty
Hand off owner    = owner, only under service execution; moves the whole tree
Governance        = Stop by role and Delete by Admin need no read; all else hidden
```

- **One evaluator.** `authorize(caller, action, resource)` replaces
  `resource_permitted`/`resource_actions`: after the role decision, one statement
  loads the resource's anchor, follows its `audience_root` to the root's policy
  and the caller's best grant there (through `access_group_members`), and
  `access` decides. A session in a collection is therefore decided at the
  session, by its own row and rights, with the collection's policy joined in:
  no collection or bot method is involved and a viewer needs only the session
  id. The managing bot and the admitted controller come from the session row;
  owner, visibility and grants come from the root. `session/list`, the bot list
  and the collection list apply the same predicate in SQL, so lists return
  readable resources only, including children listed by origin.
- **Sharing.** One method pair for every kind: `access/policy/read { resource }`
  returns owner, visibility, run-as, the audience root, grants and a revision;
  `access/policy/put { resource }` replaces visibility and the grant set, with
  the usual optional `expectedRevision` guard.
  Write members share read access or make the root universe-visible; only the
  owner adds writers or, under service execution, hands ownership to another
  member; readers change nothing. On a personal root only the owner may add a
  writer, and the share dialog offers read only. A session or bot in a
  collection has no policy of its own: reading its access shows the
  collection's, and sharing it means sharing the collection. Session, bot and
  collection creation accept the same `access` shape, plus `access.root` to
  create a session or bot in a collection the caller may write to, so a
  requester, or a service acting for one, sets the audience atomically instead
  of narrowing it after the fact; the default is `restricted` for personal
  execution and `universe` otherwise. Changes are audited (`audit: true`) and
  recorded as typed rows in `access_audit_changes` in the same transaction.
  Subjects must currently hold a role in the universe; losing universe access
  ends shared access without editing grants.
- **Content surfaces.** The policy applies to `session/read`, `session/list`,
  `session/events/read` and its long polls, run and approval reads, bot events
  and activity, and the access preview. Control actions additionally require a
  write grant.
- **Blobs.** `blobs/read` and `blobs/has` take `resource: { kind, id }` and
  succeed only when the caller may read that resource and the blob is admitted
  content of it; a resource that does not authorize the read is a refusal, never
  a prompt to search for another. Without a resource, both succeed only for a
  blob the caller uploaded. Admitted content is recorded, never derived:
  `cas_session_roots` and `cas_bot_event_roots` gain an `origin` (`content` or
  `scan`); the engine's typed extraction writes `content` rows for the references
  the runtime placed in content positions, and each structured writer (snapshot
  commit, joined context, media materialization, bot event media) writes
  `content` rows for the children it admits in the same transaction as its
  `cas_blob_edges`. Admission is where authorization happens: a reference the
  caller supplies, in run input, context append, a snapshot commit or workspace
  creation from a snapshot, profile documents or managed-session input, is
  accepted only when the caller uploaded that exact digest or may read it through
  a named source, and a manifest is admitted only when each child passes the
  same test. The generic collector keeps scanning for retention; scanned rows
  confer nothing; edges are never followed to decide a read, so a read is one
  indexed lookup. `blobs/put` records the uploader in `blob_uploads`, cascading
  with the blob; the sweeper's age cutoff already gives an upload its window
  before attachment. A format test keeps the typed extraction a subset of the
  generic collector and flags new reference-bearing fields. The garbage
  collector's "collection roots" are retention roots and get that name, so the
  word is not overloaded.
- **Bots.** `bots/create` with `access.root` puts the bot in an existing
  collection the creator may write to; without it the bot is its own root
  with the requested audience and, later, execution. The bot's anchor, its
  events and activity, and every session it creates carry that root as
  audience root; a bot's sessions also carry the bot as managing bot, which
  is the bot worker's control scope. `write` on the root invokes the bot and
  controls its sessions; trigger configuration and secrets stay with the
  owner and ManageBot as today. Deleting a bot removes its sessions as today;
  a collection is only ever removed explicitly.
- **Collections.** A collection is a root with a name, an owner, an execution
  identity and a policy, and nothing else: it exists so that sessions and bots
  share one audience and one execution identity that administrators govern in
  one place. A person creates one to share a body of work with a few people;
  a bot always lives in one; a team puts a bot and its own sessions in the same
  one; a system may create one for what it creates, or not. Contributors create
  collections and own them; Operators manage universe-visible ones; Admin may
  delete any, with everything in it. Collections route nothing and run nothing.
- **Plugins.** A workflow plugin needs no resource kind of its own. It holds a
  service principal; the sessions it creates through `session/managed/start`
  or `session/start` are standalone roots it owns, each with the `access` it
  chooses, or sessions in a collection it may write to; it is lifecycle
  controller of any of them regardless of their audiences. A lifecycle
  controller that a person's request installs acts through the API with its
  own principal and needs a write grant on that session, given at creation and
  visible in the Access panel. A tool receiver reads nothing of a session by
  default: the binding names the receiver's service principal, the invocation
  delivered to it is a readable resource for that principal (`blobs/read
  { resource: { kind: invocation, id } }` covers its arguments and their
  admitted references), and a reply's result reference is admitted into the
  session only when that principal uploaded it or it is already content of the
  session. Broader transcript access, starting runs or controlling other
  sessions come only from grants.
- **Workspaces and environments stay universe resources.** Files a restricted
  session writes into a shared workspace are as visible as that workspace, and
  `vfs/snapshots/read` stays universe-scoped until workspaces get policy rows.
  The UI says so where a session attaches one.
- **Release on delete.** Deleting a session, bot or collection removes its
  anchor, policy and grant rows with the rest of the cascade, so its id is free
  again; the id namespace belongs to the universe, not to whoever used it first.
  Admin may delete any of them, which also covers offboarded owners.
- **Web.** An Access panel (owner, running as, visibility, members) and a share
  dialog fed by the universe-scoped directory; restricted sessions and
  collections are marked in lists; a session or bot in a collection shows its
  access as the collection's; the permission preview gains `read` and `share`
  decisions.

### 2. Execution identity

- **One execution service per universe.** Universe creation also creates a
  service principal managed in that universe, assigns it Contributor, and stores
  it on the universe row as `execution_principal_id`. Universe deletion disables
  it; attribution survives. Deployments created before this slice are reset, as
  the migration baseline already requires.
- **Universe execution policy.** `personal_execution_enabled` on the universe
  row, off by default. `access/execution/read` and `access/execution/update`
  (Admin, `ManageAccess`, audited).
- **Resolution at creation.** `execution` requests `{ kind: service | personal }`
  when creating a standalone session or a collection, and a bot created with
  its own collection; a profile may request the same and confers nothing.
  `service` runs as the universe execution service. `personal` requires
  personal execution to be enabled, a user principal, and that the run-as is the
  caller; the root then defaults to `restricted`. A request that is not allowed
  is refused with `forbidden`; there is no fallback. A session or bot created in
  a collection, and every delegated child, inherits the root's execution and
  accepts no `execution` of its own. Execution stays on the anchor because it
  is immutable, reserved in the same transaction and copied through the same
  mechanism as the audience root.
- **No binding layer.** The execution identity is the principal itself, and a
  run's resource use is that principal's effective rights, nothing more. The
  next slice generalizes `execution` to `{ runAs }` with `run_as` grants on
  further keyless service principals; the anchor does not change. An execution
  principal never holds a credential: a `run_as` grant on a principal that
  holds or may hold an API key is refused, so key-level rights never reach a
  session, and a person's identity is never a `run_as` target. The only path by
  which someone else's work runs under a person's authority is the explicit
  writer on a personal root (decision 8), attributed on every run.
- **Visible.** Session, bot and collection summaries and `access/policy/read`
  expose owner, visibility, run-as and the audience root, so a viewer can show
  "shared through collection X" without a second request; the web UI shows
  "Running as" on a session and offers the personal choice only where the
  universe enables it.

### 3. Run admission, controller contexts and turn-boundary authority

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
- **Two callers, one evaluator.** Shared services authorize
  `Request(RequestContext)` and `Controller(ControllerContext)` through the same
  `authorize`. `ControllerContext { universe, actor, execution_principal, cause }`
  is today's `ControllerAuthority` with the execution principal added: a bot's
  worker binds it to the bot, and a run's activities and the shared-service
  calls they make bind it to the run's session. Reads resolve through the
  actor's audience root; control is the actor's own: itself, a bot's sessions
  (`bot` on the anchor), and the children a run admitted (`controller` on the
  anchor), never siblings under a shared root. It creates sessions only in its
  own root and uses resources only as its execution principal may. It exists
  before a run and after one, which is why it is not `RunAuthority`. Plugins
  never hold one; they are principals.
- **Delegation versus own authority.** The binding decides which relationship
  an invocation establishes, never the model's arguments: a sub-agent is
  delegation and inherits the parent's run-as; a workflow tool invokes a
  receiver that acts with its own service authority. Enforcing a delegable
  scope on children, and admitting a plugin to act under the invoker's
  authority, belong to the delegation slice.
- **Stopping standing work.** Disabling the owner of a personal collection,
  removing their universe access, disabling the universe execution service, or
  revoking a plugin's key or grants fails each subsequent admission; a bot
  records the refusal in its activity. Closing a bot cancels its in-flight work
  as today. Automatic pausing after repeated refusals is not in this slice.

### 4. Private-content access

- A universe-scoped `read_private_content` capability, assignable to a user
  principal through the existing capability changes (Admin, audited, never
  implied by a role). Holders read restricted sessions and collections they
  hold no grant on. They gain no control and no ownership; to continue orphaned
  work they fork what they may read into a session of their own, under their
  own execution.
- Each such read writes an `access_audit_events` row with `privileged = true`
  even though reads are otherwise quiet: the handler marks the request context
  when a decision relied on the capability and the boundary writes the row.
- API and CLI only; the web UI shows the capability in role editing and marks
  privileged reads, nothing more.

## Contract sketch

Illustrative shapes, not final wire signatures:

```text
ResourceAnchor    = kind + id + created_by + controller + audience_root(kind, id)
                  + [bot + execution(run_as, service | personal)]   (sessions, bots, collections)
ResourcePolicy    = owner + visibility(universe | restricted)          (roots only)
Grant             = subject(principal | group) + permission + granted_by
ResourceAccess    = anchor + root policy + caller's best grant (one statement)

Caller            = Request(RequestContext) | Controller(ControllerContext)
ControllerContext = universe + actor(kind, id) + execution_principal + cause
UniverseExecutionPolicy = execution_principal + personal_execution_enabled

authorize(caller, action, resource)      -> allowed | forbidden | not_found (+ privileged marker)
admit_run(caller, session)               -> ok | forbidden
check_turn(session)                      -> ok | authority_revoked
may_read_blob(caller, resource, blob)    = authorize(caller, Read, resource)
                                           AND admitted_content(resource, blob)
```

`access/policy/read|put`, `access/execution/read|update`, `collection/create|
read|list|update|delete`, `execution` and `access` (with `access.root`) on
session, bot and collection creation, `access` on their summaries, `read`/`share`
in the access preview, `resource` on `blobs/read` and `blobs/has` (including
`invocation`), the receiver principal on workflow-tool declarations, source
naming on attachments, `privileged` on audit events, and `authority_revoked` as
a run failure kind are the contract additions. Method `access:` and `audit:`
declarations stay mandatory.

## Persistence

Edited in place; schema revision advances once and the release metadata with it.

- `010_access_resources.sql` is the anchor: `access_resources` with
  `audience_root_kind`, `audience_root_id` and `bot_id`, `owner_principal_id`
  moved to the policy row; `run_as_principal_id` and `execution_kind`,
  optional only for profiles under a per-kind check;
  `access_resource_policies (key, owner_principal_id, visibility, revision,
  updated_by, updated_at_ms)` and `access_resource_grants (key, subject_kind, subject_id,
  permission, granted_by, granted_at_ms)` with an index on the subject, both
  referencing the root's anchor and cascading with it; `collections
  (universe_id, collection_id, display_name, revision, created_at_ms,
  updated_at_ms)`.
- `007_identity_access.sql`: `execution_principal_id` and
  `personal_execution_enabled` on `universes` (the principal foreign key
  needs that file); `001_core.sql`: `origin` on `cas_session_roots`;
  `blob_uploads (universe_id, digest, principal_id, uploaded_at_ms)` cascading
  with `cas_blobs`.
- `008_bots.sql`: `origin` on `cas_bot_event_roots`.
- `011_access_audit.sql`: `privileged boolean NOT NULL DEFAULT false` on
  `access_audit_events`; new typed change rows for sharing, hand-off and
  execution policy.
- Engine: `AuthorityRevoked` failure kind; typed content-reference extraction
  beside `collect_blob_refs`.

## Implementation order

Each step ships on its own; the order is by dependency.

1. [x] Hardening (scope 0), including the manifest enforcement test
       (2026-09-22). Poll URLs use the pinned outbound client (public
       addresses over HTTPS unless the host is a listed private network, no
       redirects); an attached grant must cover the poll URL with its
       audience, an audience-less grant needs `ConfigureResource`; endpoint
       ids inside another universe's `{universe}/…` namespace are refused;
       the universe-scoped directory holds that universe's subjects only
       (the Platform's group picker reads the deployment directory with its
       own capability); external sign-up is exercised through Better Auth's
       OAuth user-info path in the Platform live test.
2. [ ] The anchor, policy and grant rows written at reservation for standalone
       sessions and collections, bots joining collections, `collections` and
       its methods, the one-statement `authorize` with unit coverage in
       `access`; session, bot and collection lists and content surfaces enforce
       the policy; forks and clones become new roots; release on delete; Admin
       delete; hand-off under service execution only.
       Done 2026-09-22, behavior-preserving: `access_resources` +
       `access_resource_policies` + `access_resource_grants` (migration 010
       edited in place, revision stays 11), `access::authorize(Caller, action,
       ResourceAccess) -> Allowed | Forbidden | Hidden` with the full matrix in
       unit tests, `ControllerContext { universe, actor, root, cause }` as the
       controller arm, `PgAccessStore::resource_access` as the one statement
       (anchor → root policy → best grant through memberships), `not_found`
       for hidden resources, release on delete for sessions, bots and
       profiles, Admin delete, bot managers delete bot sessions. Every root
       today is a session, bot or profile with `universe` visibility; the
       live suites pass unchanged except for the two intended changes.
       Sharing done 2026-09-22: `ShareResource` action (owner or writer,
       never a role; profiles have no audience), `access/policy/read` and
       `access/policy/put` (whole visibility + grant set, optional
       `expectedRevision` against `access_resource_policies.revision`; a
       writer may not add or remove `write`; every subject must hold a role
       in the universe; the change row advances the deployment policy
       revision so a parked reader whose grant is gone revalidates against
       the session and stops), `access` on `session/start`,
       `session/managed/start` and `bots/create` applied atomically after
       reservation, `session/list` and `bots/list` filtered in SQL by the
       same rule (`store_pg::Reader`: the request's principal, or internal
       work's root), `share` in the access preview, `resource` in audit
       targets. Live matrix in `authorization_live`: owner, reader, writer
       through a group, Viewer, Operator, Admin against read, list, events,
       control, stop, delete, share and revocation on a restricted session
       and a restricted bot.
       Collections done 2026-09-22: `ResourceRef::Collection`, actions
       `CreateCollection` / `ManageCollection` / `DeleteCollection`,
       `collection/create|read|list|update|delete` (`update` replaces the
       name with the usual optional `expectedRevision`; `delete` refuses a
       collection that still holds members, so members go under their own
       rules first — a deliberate narrowing of "with everything in it"),
       `access.root` on `session/start`, `session/managed/start` and
       `bots/create` places a member in a collection the caller may write to
       (`ControlSession` on the collection), members take no visibility or
       grants of their own; a bot created without `access.root` is its own
       root and its sessions follow it, so nothing creates a collection
       implicitly (decided 2026-09-22: a collection is always something
       someone made on purpose, which is the meaning it keeps when it grows
       into projects; decision 10's "a bot always lives in a collection" is
       relaxed accordingly); `owner` on
       `access/policy/put` hands a root over (owner only, new owner must
       hold a role, previous owner keeps nothing); governance without
       content: Operator/Admin stop and Admin deletes a resource they cannot
       read (the delete is admitted, then refused by state while the session
       is open), everything else stays hidden. There is no fork or clone
       API today, so "forks and clones as new roots" is moot until one
       exists; it will reserve with the caller as controller and so be a new
       root by construction. Open: `access` on summaries (with "Running as"
       in step 4). Live matrix through HTTP:
       owner, reader, writer, group member, Viewer, Operator, Admin against
       read, list, events, control, stop, delete and share, for a standalone
       session, a universe-visible collection holding a bot, and a restricted
       collection holding a bot and a person's sessions, with the sessions and
       events under them.
3. [ ] Content: typed extraction and root origins, admission writing `content`
       rows for direct references and for the children of admitted manifests,
       `blob_uploads`, `resource` on `blobs/read` and `blobs/has`, attachment
       authorization on every path that accepts an existing reference, the
       receiver principal on declarations with invocation-scoped reads and
       reply admission.
       Mostly done 2026-09-22. `collect_content_refs` beside the generic
       collector: a ref counts as content only under a reference-bearing
       field name (`CONTENT_REF_FIELDS`: `content_ref`, `provenance_ref`,
       `arguments_ref`, `result_ref`, `blob_ref`, …), so a digest written
       into text, a preview or metadata is retained but confers nothing; a
       fixture test flags any ref outside those fields. `cas_session_roots`
       and `cas_bot_event_roots` gained `origin` (`content` | `scan`), written
       at append in the same statement as the retention roots, upgrading to
       `content` when a scanned ref is later placed as content; bot event
       refs are always content. `blob_uploads` records the API uploader;
       `vfs/snapshots/commit` records the committer as the manifest's
       uploader after admitting each child. `blobs/read` and `blobs/has` take
       `resource`: read allowed on it (hidden → `not_found`) and the blob
       admitted content of it (session roots, bot event roots, or any member
       of a collection); without a resource only engine blobs and the
       caller's uploads. Every caller-supplied reference (run input, context
       append, session start arguments, prepared-session requests, snapshot
       manifests, workspace creation from a snapshot) is admitted only if it
       is an engine blob, the caller's upload, already content of the target
       session, or, for internal work, content under the actor's root; else
       `forbidden`. Deliberate narrowings: no source naming on attachments —
       to attach what one may read elsewhere, upload it (content addressing
       makes the second put free); the receiver principal on workflow-tool
       declarations, invocation-scoped reads and reply admission are not
       implemented, since a reply reaches the workflow as a Temporal signal
       with no API boundary to check at — plugins read arguments through
       `resource: { kind: session }` as readers of the session for now, and
       the invocation resource stays open in this step. Live: a private blob's exact reference placed in the
       attacker's own session through input text, a scripted tool argument and
       a webhook payload stays unreadable through that session; an uploaded
       wrapper whose children are another session's content gives its uploader
       the wrapper only; an upload is readable by its uploader only; attaching
       a foreign reference is refused; a receiver replying with a reference it
       did not upload is refused.
       Live-test repair: runtime-authored controller documents bypass the
       caller-upload admission check so bot tool declarations can bootstrap;
       request admission and controller blob reads retain their checks.
       Context and snapshot fixtures upload through the public API. Live
       worker harnesses bound each client body to three minutes, including
       in-flight API calls, then bound worker shutdown separately and retain
       the original failure. Workflow-plugin polling propagates API failures
       immediately; rejection tests wait for signal processing, and the
       cancellation fixture finishes only on cancellation instead of racing
       a fixed sleep. Channel teardown cancels typing heartbeats and drains
       admitted bot work before stopping session workers. Sub-agent cleanup
       waits for the supervisor's terminal state before terminating child
       workflows, so close activities can receive their acknowledgements.
       Large transfer futures are heap-pinned to fit the current-thread test
       stack. Verified 49 live tests across sessions, bots, sub-agents, VFS
       transfers, workflow plugins, channels, Platform identity, revocation
       and authorization, including provider-backed cases; suite runtimes
       ranged from 5 to 154 seconds, excluding compilation. The final
       sub-agent rerun used an isolated snapshot to preserve concurrent
       engine/API edits in the working tree. The
       production activity-timeout proof retains an explicit larger budget
       in `runs_live_slow`.
4. [x] Universe execution service, execution policy methods, execution
       resolution at creation and inheritance under roots, "Running as" in
       summaries and web.
       Done 2026-09-22 (web deferred to step 7): `run_as_principal_id` +
       `execution_kind` on the anchor (nullable only for profiles), copied
       from the root to members and children; `execution_principal_id` +
       `personal_execution_enabled` on `universes` (added from
       `007_identity_access.sql`, where the principal table exists). The
       execution service is a keyless service principal managed in the
       universe holding Contributor, created on first use rather than at
       universe creation so every creation path (`deployment/universes/
       create`, `ensure_universe`, local development) is covered; universe
       deletion disables it. `access/execution/read|update` (ManageAccess;
       update audited and advances the policy revision). `execution:
       { kind }` on `session/start`, `session/managed/start`, `bots/create`
       and `collection/create`; `personal` requires the universe to enable
       it and a user principal, binds `run_as` to the creator and defaults
       the root to `restricted`; a member of a collection inherits and
       refuses a choice; hand-off is refused under personal execution.
       `access` (`ResourceAccessSummary`: root, owner, visibility, execution)
       on `SessionView`, `SessionSummaryView`, `bots/read`, `bots/list`
       items and `CollectionView`, joined into the session list and bot
       roster queries so a list costs no extra lookups; `execution` on
       `access/policy/read`. A profile requesting an execution default is
       not implemented; profiles confer nothing either way.
5. [x] `admit_run`, `RunAuthority` on the run record, the `llm_generate` turn
       check, `AuthorityRevoked`, the controller context with its execution
       principal, and bot/sub-agent migration onto it.
       Done 2026-09-22: every run start (API, bot, sub-agent, workflow tool)
       goes through `start_run_internal`, which after the requester's own
       control check calls `admit_run`: the session's execution principal
       must be active with `UseResource`, else `forbidden`; a bot records
       that refusal on the fire like any other. `RunAuthority` on the run
       record was implemented and then removed (2026-09-22): nothing
       decides on it, `runAs` never varies within a session (it is the
       anchor's execution, on every view as `access.execution`), and "who
       asked" for an API start is the audit row `session/runs/start` writes;
       an audit row for controller-initiated runs is the cheaper place
       should that ever be wanted. The turn check lives in the
       `llm_generate` activity: before each model call it reads the session's
       anchor and the run-as principal's effective rights; when they no
       longer hold it returns `LlmGenerationStatus::AuthorityRevoked`, which
       the engine turns into `TurnOutcome::Failed { kind: AuthorityRevoked }`
       and `RunFailureKind::AuthorityRevoked` (`authority_revoked` on the
       wire); the session stays open. `ControllerContext` carries the actor's
       execution principal and resource use follows it rather than the
       actor's kind. Not done here: the mid-run live scenarios below, which
       need a multi-turn fake model; admission refusal and recorded authority
       are covered in `authorization_live`. Live: disable an owner
       mid-run and require the current turn to complete and the next to fail
       with the kind; a sub-agent child failing after its parent's principal is
       disabled; a personal bot refusing admission after its owner is disabled;
       a controller context bound to one root failing to read another root that
       runs as the same execution service; a run in a collection failing to
       steer a sibling session; a hand-off of a service collection reaching
       every session and bot in it and leaving the previous owner nothing.
6. [ ] `read_private_content`, privileged reads and their audit rows.
7. [ ] Web: Access panel, share dialog, restricted markers, execution choice,
       collection pages, workspace visibility note; Platform mapping of the new
       methods; docs (`multi-tenancy.md`, `authentication-and-tenancy.md`, API
       reference). User documentation is deferred to this step: steps 2–6
       change no `docs/documentation` page beyond the generated API reference,
       and the sharing paragraphs already there describe the state after
       step 2's sharing part.

## Validation

- Unit: access decisions for every visibility × grant × role combination for
  sessions, bots and collections, with a missing policy row denying; only the
  owner adds a writer to a personal root, and that writer's run records the
  owner as run-as and the writer as authorizer; hand-off refused under personal
  execution; execution resolution refuses without fallback and is refused
  inside a collection; typed extraction is a subset of the generic collector on
  every event fixture; audit target extraction for the new methods.
- Live (disposable services, serialized): the HTTP access matrix above; the
  content cases in step 3; a fork by a reader yielding a new root owned by the
  reader; execution recorded on run records and visible in summaries;
  turn-boundary revocation for API, bot and sub-agent runs; the bounded
  controller context; privileged reads writing exactly one row each; hardening
  cases (audience mismatch, private network, missing auth mode, real external
  sign-up, directory scoping, error body).
- Two plugin fixtures through the public contracts, with no plugin-specific
  branch in the session worker: a tool-only receiver serving a restricted
  session, which reads its arguments through the invocation, replies with its
  own upload, and can read nothing else of the session; and a long-lived
  controller with one service key that creates and coordinates two sessions
  with different audiences and is refused on a third it does not own.
- Measured: an ordinary read stays at one key read, one rights read and one
  access lookup; a blob read adds one indexed lookup; a model call adds one
  rights read.

## Boundary and follow-up

Not in this slice: further execution principals with `run_as` grants, policy
rows and permission vocabularies for workspaces, environments, MCP servers and
grants (next slice, on the same anchor; `vfs/snapshots/read` then takes a
workspace context), invoke-only bot grants and conversations private to their
invoker, collection surfaces beyond creation and the Access panel, a plugin
acting under the invoker's authority or holding an internal execution context
of its own, requester propagation to first-party MCP servers
such as the Configurator, SSO and provisioning, personal event-driven
automation, source-imposed audience limits, audit retention and export, a
two-person rule or UI flow for privileged access, and effect-time checks inside
adapters.

After this slice the parent sequence continues with resource restrictions and
delegation enforcement, then SSO and provisioning, whose offboarding must govern
API keys and the standing bot and collection authority defined here.

Current seams: [authorization service](../../crates/temporal-server/src/gateway/service/authorization.rs),
[resource store](../../crates/store-pg/src/resources.rs),
[reference collector](../../crates/engine/src/storage/blobs.rs),
[model-call activity](../../crates/temporal-server/src/worker/activities/llm.rs),
[bot worker](../../crates/temporal-server/src/worker/bots.rs),
[Platform gateway](../../platform/server/src/routes/gateway.ts).
