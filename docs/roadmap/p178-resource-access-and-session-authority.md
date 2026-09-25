# P178 — Resource access and session authority

**Status:** Implemented, 2026-09-24; proposed 2026-09-23 and rewritten the same
day after review. Decisions 3 to 8 superseded for 0.1 by
[core: universes, keys and actors](p179-core-universes-keys-and-actors.md) and [platform: organizations, roles and unshared work](p180-platform-organizations-roles-and-unshared-work.md). Third
slice of [enterprise authorization](later/pNNN-enterprise-authorization.md),
building on [identity and universe authorization](p176-identity-and-universe-authorization.md),
[session access and execution authority](p177-session-access-and-execution-authority.md)
and [session config attachments](p173-session-config-attachments.md). Together
with those two slices it is the version 0.1 permissions system: the point at
which the `permissions` branch merges and production work starts depending on
the model.

Lightspeed is greenfield. Migrations are edited in place and contracts reshaped
where the model needs it. Keep the vocabulary small and enforce it on every
supported path; misconfiguration is part of the threat model.

## Outcome

A person can restrict a workspace, an environment or an MCP server to selected
people, groups or agent identities. A session uses only the resources its
execution identity may use, narrowed by its own setup. Changing a profile,
supplying a resource id, calling the API directly, or starting background work
cannot get around that, and losing access stops work at its next turn.

> A person controls a session. The session runs as an identity. The identity
> may use resources. Session setup narrows what this session does with them.

Every refusal names the acting identity, the resource, the action and where the
permission would have come from, without disclosing a hidden resource.

## Starting point

Implemented by the previous slices:

- Principals, groups, universe roles, capabilities, scoped keys, audited
  identity changes.
- One anchor per session, bot and profile; a root policy (owner, visibility)
  and grants (`read`, `write`); one evaluator, `authorize(caller, action,
  resource)`, reading anchor, root policy and best grant in one statement.
- A fixed execution identity per root: the universe's keyless execution
  service, or the creating person. `admit_run` and the model-turn check require
  that identity to hold `UseResource` in the universe.
- Attachments with access ladders (workspace `read`/`edit`, environment
  `read`/`edit`/`exec`/`jobs`, MCP tool subsets) enforced deterministically by
  the engine.
- Content admission for blobs, privileged reads, two audit streams.

Missing: any per-resource restriction. Every Contributor, and therefore every
session, may use every workspace, environment and MCP server in the universe,
and the execution service holds Contributor, a role written for people.
Collections were built in P177 and their UI removed again; the API, store and
evaluator arms remain without a way to reach them.

## Decisions

### 1. Three layers, one evaluator

| Layer | Answers |
| --- | --- |
| Universe role | Is this identity eligible to see, use, create or configure resources of this kind at all? |
| Resource policy | May this identity use or configure this particular resource? |
| Admitted session scope | Which resources, and which operations on them, has this session been set up with? |

For an operation a session performs:

```text
allowed = role eligibility of the execution identity
          AND resource policy for the execution identity
          AND admitted scope of the session
```

A direct API call uses the caller's own identity and has no session scope.
Internal work takes its execution identity from the session's anchor, never
from request arguments or model output. The caller's right to control the
session is a separate check and stays as it is. Roles and grants never add an
attachment to a session; a session only gets what its setup requested and the
runtime admitted.

### 2. Executor: the role of agent identities

Contributor is a role for people. If the execution service holds it, every
future Contributor power reaches every agent in every universe. Agent identities
therefore get their own system role:

- **Executor** is a universe-scoped role allowing exactly `Read` (see
  universe-visible resources) and `UseResource`. It allows nothing else: no
  creation, control, configuration, sharing or identity administration.
  Internal controller rules keep authorizing bot and session lifecycle.
- The runtime assigns it to the universe's execution principal when that
  principal is created; `deployment/identity/apply` refuses to assign or revoke
  it; the Members role picker does not offer it. The principal is identified
  structurally, by `universes.execution_principal_id`, never by name.
- It stays keyless: `may_issue_key` refuses any principal holding Executor.
- It appears as **Default agent identity** in execution settings, in resource
  Access dialogs as a grantable subject, and in effective-access explanations;
  ordinary member editing and removal do not show it.

Personal execution is unchanged: the run uses the person's own roles and
grants. A later narrower agent identity is one more keyless Executor principal
with its own grants and a `run_as` grant deciding who may run as it; the
anchor does not change. Not in this slice.

### 3. Three resource kinds, one grant permission

| Kind | `use` covers | Created by |
| --- | --- | --- |
| Workspace | reading files, snapshots and blobs of it; committing and advancing its head | Contributor, who owns it |
| Environment | attaching and activating; files, processes, jobs; the credentials it injects | Operator, who owns it |
| MCP server | discovering and calling the tools its record allows | Operator, who owns it |

Every one of them gets an anchor and a policy row in its creation
transaction, with visibility `universe` or `restricted` and grants to principals
or groups, exactly like a session. The only grant permission on these kinds is
`use`; `read` and `write` stay the vocabulary of session and bot roots, and
`access` validates the permission against the kind. Seeing a resource (lists,
metadata, workspace contents) follows visibility: universe-visible resources
are seen by every member, restricted ones by their owner, grantees and Admin.
A missing policy denies, and a hidden resource returns `not_found` on every
surface, as sessions do today.

Role defaults on a universe-visible resource:

| Action | Viewer | Contributor | Operator | Admin | Executor |
| --- | --- | --- | --- | --- | --- |
| See | yes | yes | yes | yes | yes |
| Use | | yes | yes | yes | yes |
| Configure | | owner | yes | yes | |
| Manage access | | owner | owner | yes | |

On a restricted resource the role allowance is replaced: see and use need
ownership, a `use` grant, or Admin; configure and manage access need ownership
or Admin. This is the rule the evaluator already applies to bots, `owner ||
(by_role && universe_visible)`, extended to three more kinds. There is no
configure grant, no manage-access grant, no deny rule and no inheritance
between resources. Roles stay eligibility ceilings: a Contributor with a grant
still cannot configure, and delegating the maintenance of one resource is an
ownership hand-off through the same dialog, not a promotion. One permission per
kind also keeps the store's "best grant" lookup a single `max`; a typed action
set with implications is not needed until a kind has two independent grants.

Restriction of an operational resource is governance, not privacy. Admin may
grant itself `use`, and the change is audited like every policy change; private
session content keeps its own rules and `read_private_content` stays the only
way past them. Manage access is distinct from Configure: changing an endpoint,
a credential binding or a tool allowance is configuration; changing who may use
the resource is not, and Configure never implies it.

**Configure** means, per kind: workspace rename and delete; environment
lifecycle, power, idle policy, ingress, credential bindings and registration;
MCP server record, credential binding, tool allowance, execution and approval
settings, and delete. Advancing a workspace head is `use`; `vfs/workspaces/
update` therefore requires `use` when it moves the head and Configure when it
renames.

**Credential binding.** Binding a credential source to an environment or MCP
server requires Configure on the destination and follows the rule P177 set for
poll triggers: the source's audience must cover the destination, and an
audience-less source additionally requires `ConfigureResource` at universe
level. `auth/grants/*` are not a resource kind and get no policy rows. Use of a
resource never exposes the secret; the existing audience, status and exposure
checks remain. Binding delegates the source to the destination's current and
future audience: widening the destination's visibility later is a manage-access
change on the destination, made by its owner or Admin, and does not revalidate
the binding. Whoever may configure a resource is trusted with what sessions
send to it, since an endpoint or credential change redirects future requests;
this slice audits such changes and does not promise more.

**The Configurator is the first restricted resource.** Its installer creates
an Operator service whose key backs a universe MCP server with approval set to
`never`; anyone whose session can attach it drives Operator actions. The
installer registers it `restricted`, owned by the installing Admin, and the
setup text says that granting it to Default agent identity hands it to every
default-service session. The intended way to use it in 0.1 is personal
execution by a grantee; a narrower agent identity is later work.

**Model connections stay on role rules.** They are `auth_providers` rows with
a deployment-side fallback to environment-configured keys; a policy that an
absent row bypasses would be worse than none. They become a kind once that
fallback is an explicit, governed connection. Environment providers, templates
and registration keys likewise stay on role rules.

### 4. Session scope is admitted, then enforced

On every path that installs attachments (`session/start`, `session/managed/
start`, `session/config/put`, profile application, `bots/create` and update,
sub-agent spawn), the runtime resolves each attached workspace, environment and
MCP server in the universe and requires `use` for the session's execution
identity. A refusal is typed and names the resource and the identity; a
resource the caller may not see is `not_found`. Nothing is dropped, elevated or
partially committed, and no other identity is chosen. Removing an attachment is
always admitted, so a session that lost a resource can be repaired.

Bot exec polls that name an environment, directly or through the profile's
default attachment, are admitted the same way when the trigger is configured
and again at each fire; a refused fire is recorded on the bot as other
admission refusals are.

Delegated children run as the parent's execution identity and their profile's
attachments are admitted against it. Narrowing a child to its parent's admitted
scope is later work: it is a scope question, not an identity escalation, since
the child can use nothing its identity could not use directly.

Access ladders, working directories, MCP tool subsets and selection tools stay
exactly as P173 defines them; the engine keeps enforcing them deterministically.
A ladder is a tool setting, not an OS boundary: a process on a machine has that
machine's authority.

### 5. Revocation stays per turn

P177 decision 1 holds: authority is checked at run admission and at the start
of every model call, and adapters check nothing. Both checks now evaluate, in
one statement, `use` for the execution identity on every resource the session
has attached, in addition to the identity's own status and `UseResource`
eligibility. When any of them fails, `admit_run` refuses, or the turn returns
`AuthorityRevoked` and the run fails with that kind while the session stays
open. The workflow supplies the attached resource references to the turn check
so the activity does not read session configuration.

A turn that was authorized completes: its tool batch, awaits and retries run
under the authority it started with. Direct API reads and streams are checked
per request; parked long polls revalidate on the policy revision as they do
today. Nothing here promises immediate process termination, retraction of
delivered credentials or data, or cancellation of external effects; a queued
run is refused at admission, a running turn is not interrupted.

### 6. Direct API paths

Methods that name a resource authorize against it with the caller's own rights:

| Methods | Requires |
| --- | --- |
| Workspace read/list, file read, snapshot read with a workspace context, blob read with `resource: { kind: workspace }` | see |
| Head move, environment activate/deactivate for a session, environment job create/cancel, MCP tool discovery for a session | `use` (plus the existing session control check) |
| Workspace rename/delete; environment create/close/register/power/idle/ingress/credential bindings/registration keys; MCP record put/delete, auth and tool discovery for configuration | Configure |
| `access/policy/read`, `access/policy/put` on the three kinds | see; manage access |

Lists filter by visibility in SQL through the existing reader predicate. A
blob readable through a workspace is content of its current head or base
snapshot; other snapshots need their uploader. A snapshot reference alone
confers nothing, as P177 already decided for session content.

### 7. UI and explanations

- The shared Access dialog serves workspace, environment and MCP server pages
  with the `use` permission; owner, visibility, grants and hand-off behave as
  for sessions. Granting **Default agent identity** shows what it means: every
  session and bot running as it may use the resource.
- Attachment pickers in session and bot setup list what the selected execution
  identity may use; a resource the person may use but the identity may not is
  shown disabled with the reason, for example `Default agent identity cannot
  use Production. Run this session as yourself or ask for access.`
- `access/read` accepts the three kinds and returns see, use, configure and
  manage decisions with their source (role, ownership, direct grant, group).
- Restricted markers, Running as and revocation failures use the existing
  patterns. UI visibility is a convenience; the runtime decides.

### 8. Policy writes reauthorize inside the transaction

Today `access/policy/put` checks owner and writer rules in the handler, then the
store locks the policy row and rechecks only the revision and subject
membership. A writer revoked between the two steps commits a stale grant set
unless the request carried `expectedRevision`, which the direct API does not
require. The store takes the actor and re-derives owner and best grant under the
row lock, refusing when the actor no longer holds the right the change needs;
`expectedRevision` remains an edit-intent guard, never the access control. The
same discipline applies to hand-off and to every policy write this slice adds.

## Contract sketch

```text
Role              += Executor                       (system-assigned, keyless)
ResourceRef       += Workspace | Environment | McpServer
ResourcePermission += Use                          (operational kinds only)
UniverseAction    += CreateWorkspace; UseResource, ConfigureResource and
                     ShareResource take a resource when one is named

authorize(caller, UseResource, workspace)        -> allowed | forbidden | not_found
admit_run(session)   = identity active AND UseResource AND use on each attachment
check_turn(session, attachments)                 -> ok | authority_revoked
```

`access/policy/read|put` and `access/read` accept the new kinds; workspace,
environment and MCP server views carry `access` (owner, visibility, execution
absent); creation and configuration methods return the typed refusal above.
Method `access:` and `audit:` declarations stay mandatory. Collection methods,
`access.root` and `ResourceRef::Collection` are removed from the contract.

## Persistence

Edited in place; schema revision advances once with the release metadata.

- `007_identity_access.sql`: `executor` in the role check constraint.
- `010_access_resources.sql`: nothing structural; kinds are text. The
  `collections` table is dropped.
- Anchor and policy rows are written in the same transaction as
  `vfs_workspaces`, `environments` and `mcp_servers` rows, with the creator as
  owner and `universe` visibility unless the request says `restricted`;
  deletion cascades them like sessions.

## Implementation order

Each step ships on its own; the order is by dependency.

0. [x] Hardening: reauthorize policy replacement and hand-off under the row
       lock (decision 8), with a deterministic revoke-versus-put test.
1. [x] Remove collections: methods, `access.root`, the kind, evaluator arms,
       store paths, table, web fixtures and documentation. The `audience_root`
       mechanism stays for delegated children and bot sessions.
2. [x] Executor: role, system assignment at execution-principal creation,
       refusal in identity changes and key issuance, contract regeneration,
       Members and execution-settings UI, matrix tests.
3. [x] Kinds: anchors and policy rows for workspaces, environments and MCP
       servers written at creation; `use`; evaluator arms with the full
       role × visibility × grant matrix in unit tests; `access/policy/*` and
       `access/read` on the kinds; list filtering; `CreateWorkspace` for
       Contributors.
4. [x] Enforcement: attachment admission on every installing path and on bot
       exec polls; `admit_run` and the turn check over attachments; direct API
       reclassification from decision 6; typed refusals.
5. [x] Configure and manage-access rules, the credential-binding rule, Access
       dialogs on the three pages, pickers with reasons, demo fixtures, user
       documentation, and the merge of the `permissions` branch.

## Validation

- Unit: for each kind, every role × visibility × grant × ownership combination
  for see, use, configure and manage access, including Executor and a missing
  policy; Executor refused in identity changes and key issuance; `use` refused
  on session roots and `read`/`write` on operational kinds.
- Live (disposable services, serialized): a Contributor's session attaching a
  restricted environment by id is refused, and the same session under personal
  execution by a grantee succeeds; a granted default identity lets any session
  use the resource; revoking `use` mid-run lets the current turn finish and
  fails the next with `authority_revoked`, and the next run is refused at
  admission until the attachment is removed; a bot poll on a restricted
  environment is refused at fire; jobs, files, blob and snapshot reads on a
  hidden workspace return `not_found` through the API; Configure without
  manage access cannot change visibility or grants; an audience-less credential
  cannot be bound by an Operator without universe `ConfigureResource`.
- Live, Configurator: installed restricted; a Contributor's default-service
  session cannot attach it; the same person under personal execution with a
  `use` grant can; granting Default agent identity opens it to every
  default-service session, and the dialog said so.
- Concurrency: a writer revoked after reading the policy cannot commit a
  replacement that restores its own grant; a former owner cannot complete a
  hand-off.
- Measured: a model call still adds one statement; an ordinary resource read
  stays at one access lookup.

## Later

Model connections as a kind after the environment-key fallback is removed;
per-dispatch checks inside adapters; narrowing delegated children to the
parent's admitted scope; further agent identities with `run_as` grants;
read-only human grants on workspaces; per-person MCP tool policies; path-level
rules; explicit deny; approvals; SSO and provisioning; projects, if and when a
shared root is needed again.

## Current seams

- [Authorization types and evaluator](../../crates/access/src/lib.rs),
  [resource policy store](../../crates/store-pg/src/resources.rs).
- [Shared-service authorization](../../crates/temporal-server/src/gateway/service/authorization.rs),
  [session preparation](../../crates/temporal-server/src/gateway/service/session_preparation.rs),
  [MCP preparation](../../crates/temporal-server/src/gateway/service/mcp_api.rs),
  [bot fires](../../crates/temporal-server/src/bots/fires.rs).
- [Turn check](../../crates/temporal-server/src/worker/activities/llm.rs),
  [credential resolution](../../crates/temporal-server/src/worker/secrets.rs),
  [environment jobs](../../crates/temporal-server/src/worker/activities/environment_jobs.rs).
- [Attachment types](../../crates/engine/src/core/components/config.rs),
  [setup editor](../../platform/web/src/components/session/session-config-editor.tsx),
  [Access dialog](../../platform/web/src/components/access/access-dialog.tsx).
