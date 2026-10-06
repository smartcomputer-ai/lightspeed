# Session closing, deletion, and audit

Status: implemented and validated.

## Policy

Closing ends execution and retains readable history. Soft deletion makes a closed
session disappear from every ordinary universe interface, including admin views.
Within Platform, only Platform administration can inspect deleted-session
metadata and permanently remove the retained session records. Direct runtime
clients can permanently delete closed sessions without first soft-deleting them.

| Role | Close | Soft delete | Permanently delete |
| --- | --- | --- | --- |
| Viewer | None | None | None |
| Contributor | Own unshared sessions | None | None |
| Operator | Own sessions and universe-shared sessions | Shared sessions | None |
| Universe admin | All sessions in their universe | All sessions in their universe | None |
| Platform admin | All sessions | All sessions | Already soft-deleted sessions |

Ownership and visibility use the existing audience root, including delegated
sessions. Contributors cannot close shared sessions, even their own. Operators
cannot delete their own private sessions. Cancelling a run remains separate from
closing a session. Force-close uses the same permission as close. Existing
controller rules still apply to managed sessions.

## Storage and deletion

`session/delete` permanently deletes by default. With `softDelete: true`, it sets
`sessions.deleted_at_ms` instead. Platform forces that option server-side for
every member, including admins; retention also explicitly chooses soft deletion.
The CLI defaults to permanent deletion and exposes `--soft-delete`.
There are no tombstone or runtime audit tables. Every selected session must
already be closed. Existing leaf and explicit cascade behavior remains:
history forks and delegated descendants are
included; permanent cascades also include already soft-deleted descendants so
retained forks cannot lose their source history. Configuration-only clones
remain independent. An atomic shared-only guard prevents operator cascades
from deleting private forks.

Normal reads, lists, access lookups, mutations, event reads, and source-based
creation exclude deleted sessions. Universe admins get no trash view or
include-deleted switch. Retention deletion uses this same soft-delete operation;
its scheduling remains admin-only. Retained events, checkpoints and CAS roots
remain intact until explicit permanent deletion. Soft-deleted rows prevent reuse
of their session IDs while retained.

Purging already soft-deleted sessions is a deployment-scoped operation, exposed
in Platform only to platform admins through the Universes administration page
or an unchecked-by-default
“Also permanently delete retained history” option in the ordinary session delete
dialog. Both paths use the same audited Platform purge endpoint. The inline path
soft-deletes first, then purges; a failed purge leaves a retry available without
repeating soft deletion. In the Universes modal, admins can select multiple
deleted sessions or choose Purge all. Purge all collects every page before
confirmation; newly deleted sessions arriving afterward are not added to that
selection. Each purge uses the existing audit path, and partial failures can be
retried. The admin confirms removal of the selected sessions and their deleted
history subtrees. The operation
rejects any selected session that has not been soft-deleted. Repeating it after
successful removal returns no affected IDs. There is no automatic purge, grace
period, tombstone, restore API or restore UI.

Deleting session rows cascades to their event log, checkpoints and blob roots.
Shared blobs stay protected by their remaining references; unreferenced blobs
follow ordinary garbage collection. This does not delete attached workspaces,
environments, external exports, backups or Temporal history. Direct blob access
keeps its existing universe-level boundary. Session deletion is not a guarantee
of erasing every copy of the content.

Deleting a bot closes its managed sessions and retains their history. Recreating
a bot starts fresh session generations beyond any retained generations.

## Authorization and API keys

Platform enforces the role and audience rules on the server, including direct
HTTP requests, and mirrors them in individual and bulk UI controls. The runtime
continues to enforce service-key scopes and method groups, not human memberships.

`session/delete` (either mode) and `session/retention/put` require the explicit
`session/delete` key group. Existing ordinary `session` keys do not silently
acquire deletion access. Administrators deploying this change must provision that capability on
service keys that need it. Deleted-session administration requires a deployment
key with `deployment/sessions`; Platform independently requires platform admin.

Deletion retention is admin-only even though operators can immediately delete
shared closed sessions. Non-admin creation overrides profile deletion schedules
with no deletion schedule, and rejects an explicit deletion schedule. Bots also
avoid inheriting profile deletion schedules. This prevents configuration from
becoming an indirect way to delete private sessions.

## Platform audit

For sessions, Platform audits only successful permanent deletion in its existing
`identity_audit` table. Records contain the actor, universe, target session,
action, timestamp and every removed session ID. They survive deletion of session
data. A separate Platform admin Audit log page displays these events alongside
access changes. No second audit table or runtime audit API is introduced. Session creation, sharing, close, force-close, retention-policy changes
and soft deletion do not create audit records. Existing access/security auditing
remains unchanged.

Audit is scoped to requests through Platform. Direct runtime API calls and
background retention work do not create Platform audit entries. Runtime mutation
and Platform audit insertion use separate databases: a mutation may succeed even
if the subsequent audit write fails. There is no cross-database transaction,
outbox or audit bridge in this implementation. Failed mutations are not recorded
as successful actions; empty purge retries do not duplicate successful purge logs.

## Validation and progress

- [x] Define close, soft-delete and permanent-delete authority.
- [x] Implement the soft-delete flag and hidden-session storage behavior.
- [x] Preserve bot histories and protect retention admission paths.
- [x] Reuse Platform audit and add platform-admin inspection and explicit purge.
- [x] Align individual, bulk and demo lifecycle controls.
- [x] Complete regression coverage, regenerated contracts and component checks.
- [x] Align existing user, access and operations guides with deletion and audit behavior.
- [x] Restore permanent deletion by default for direct runtime clients, with explicit soft deletion in Platform and retention.

Validation passed: workspace Clippy with warnings denied; API, PostgreSQL-store
and runtime unit tests; local PostgreSQL lifecycle and CAS-retention tests; the
serialized live bot history/recreation test; full backend, web, SDK and Configurator
tests; TypeScript checks; production web/demo builds; and release metadata checks.

Direct-deletion follow-up validated with API schema/default tests, PostgreSQL
lifecycle and blob-retention tests, both deletion modes through the live runtime
API and CLI, Platform permission/audit regressions, SDK and Configurator tests,
TypeScript checks, documentation checks and workspace Clippy.
