# P206 — Universe deletion

**Status:** Slices 1–5 implemented and validated, 2026-11-16.
The optional provider-wide resource release remains deferred (slice 6).

This comes before
[self-service universes](p207-creators-and-self-service-universes.md)
(creator keys, ownership, deletion protection and expiry). That work makes creating
and deleting universes routine, including test runs that create dozens of them,
so deletion has to be complete first.

## Goal

Deleting a universe removes everything Lightspeed runs, stores and pays for on
its behalf, whatever state the universe is in:

- One call, with no preparation. Sessions may be open, runs in flight, bots
  scheduled, daemons connected and machines provisioned.
- Nothing has to be closed or archived beforehand. Deletion closes the
  universe's environments itself.
- Telling providers and daemons is best effort. An unreachable provider or a
  disconnected daemon never keeps a universe from being deleted.
- From the moment the call returns, the universe accepts no requests and its
  handle is free for a new universe.
- Repeating the call is safe. An interrupted deletion resumes; a deleted
  universe is never brought back.
- Creating and deleting fifty universes in a loop leaves no rows, workflows,
  schedules, objects or machines behind.

Who may delete does not change here: a deployment key with
`deployment/universes`, and through the Platform a Platform admin.

## Starting point

Before this work, `delete_universe` in
`crates/temporal-runtime/src/gateway/deployment.rs` did three things in one request: it terminated the workflow of every session row,
deleted the universe's objects from the object store, and deleted the
`universes` row, from which every universe table cascaded. It then dropped the
universe's cached state in the process that served the request.

What that implementation left behind or got wrong:

| Area | Before implementation |
| --- | --- |
| Requests during deletion | Nothing refuses them. The Platform requires the universe to be archived first, but the runtime has no such state, so API clients, bots and daemons keep working until their rows disappear. |
| Other processes | Only the serving process drops its cached state. Other gateway and worker processes keep serving the universe from cache until idle eviction, up to four hours. A cache hit never re-checks that the universe exists. |
| Resurrection | Building a universe's state calls `ensure_universe`, an `INSERT … ON CONFLICT DO NOTHING`. A cache miss that races the delete creates the row again. `deployment/universes/create` is idempotent by UUID, so it also re-creates a deleted universe. |
| Bots | Controller workflows and trigger Schedules keep running. The bot schedule reconciler walks the universes that exist, so it never visits a deleted one, and the Schedules keep firing. |
| Channels | Conversation workflows never complete and have no rows; they stay open forever. |
| Workflow-tool executions | Sub-agent, code and workflow-tool job executions have the Temporal workflow id `wtx:sha256:…`, which does not name the universe. Terminating the session that started them runs no workflow code, so they are never cancelled. A sub-agent execution then polls a universe that no longer exists, and treats every error as transient. |
| Environment jobs | `EnvironmentJobWorkflow` keeps polling after its environment is closed or its universe is gone, recording the error and trying again. |
| Provisioned environments | The cascade removes the rows without calling the provider. Nothing sends `target/close`; the machines keep running and the provider has no listing by which the runtime could find them again. The Incus provider also keeps a project, network, ACL and profile per universe. |
| Registered daemons | With a single gateway process, the control connection of a daemon whose row vanished is not ended: the watcher logs the missing row and keeps waiting. The daemon is never told. |
| Object store | The row is deleted before the universe's object prefix is cleared, and a failure to clear it is only logged. Those objects are then unreachable. |
| Platform | Rows are removed only by the Platform's own delete route. A universe deleted through the runtime stays in the Platform as "missing", with a repair action that creates it again. |
| CLI | There is no `lightspeed universe delete`. |
| Tests | Live tests cover the row cascade, keys and the handle. None covers bots, channels, schedules, workflow-tool executions, environments or a second process. |

## Design

### Three states

A universe is **active**, **deleting** or **deleted**.

- `deployment/universes/delete` moves it to *deleting* in one statement and
  returns. This is the fence: it is immediate and it is the only synchronous
  part.
- A background pass tears the universe down step by step. Every step is
  derived from what still exists, so the pass can stop anywhere and start
  again.
- The last step deletes the row and records the UUID as *deleted*.

Deletion is asynchronous because closing a provisioned machine is a
conversation with a provider, and a universe may have thousands of workflows
to stop. A request that waits for all of it would time out exactly when it
matters. Callers that want to wait poll `deployment/universes/read`; the CLI
does that with `--wait`.

### The fence

Once a universe is deleting:

| Caller | Result |
| --- | --- |
| A universe key of that universe | `unauthenticated`, as after deletion today. |
| A deployment key naming it by UUID for a universe operation | `notFound`. |
| Anything naming it by handle | The handle no longer resolves. It is free at once, so a new universe can take it while the old one is still being torn down. |
| `deployment/universes/read`, `list` and `delete` by UUID | Work, and show the deletion in progress. |
| `deployment/universes/create` with the same UUID | `conflict`. |
| A daemon registering or reconnecting | Refused as an unknown registration key. |
| A worker activity for the universe | Fails as an unknown universe, without retry. |

The gateway does not rely on caches for this. Key resolution and universe
resolution already read the database on every request; both gain the check.
The cached per-universe state changes in three ways:

- Building it no longer inserts the universe row. Only explicit creation and
  the single-universe development start-up create universes.
- A cache miss treats a deleting universe as unknown.
- A cached entry is re-validated against the row when it is older than 30
  seconds, so a worker process stops within that bound even if it misses the
  signal described under [Other processes](#other-processes).

Passes that iterate universes (the promise reaper, session retention, the
blob sweep and the bot schedule reconciler) skip deleting universes. Without
that, the schedule reconciler would create the Schedules again.

### The deletion pass

One pass runs on the session worker role beside the existing reapers, under
its own leadership lock, every few seconds and at once when a deletion is
requested. For each deleting universe it works through four steps in order and
moves on only when a step has nothing left to do. The step it is on, and what
it is waiting for, are stored on the row and returned by `read`.

**1. Intake.** Delete the Temporal Schedule of every bot trigger, using the
ids derived from the trigger rows. Schedule reconciliation and intake share
a per-universe lock, so an upsert admitted before the fence cannot recreate a
Schedule after intake removes it. Delete the universe's channel account rows,
so connector hosts retire those accounts at their next discovery.

**2. Workflows.** List the universe's open workflow executions in Temporal by
workflow id prefix (`WorkflowId STARTS_WITH "<universe>/"`) and terminate each
one. Repeat until two consecutive listings are empty; visibility is eventually
consistent, and an execution started just before the fence can appear late.
This replaces terminating by session row, and it needs every workflow id to
begin with the universe:

- The Temporal workflow id of a workflow-tool execution becomes
  `<universe>/<execution id>`. It is composed where the execution is started,
  cancelled and signalled. The harness's execution id does not change, and
  plugin-declared workflow types are covered because the runtime composes the
  id.
- A test asserts that every id composer produces the prefix, so a new workflow
  type cannot be added without it.

Two workflows also learn to stop by themselves, which fixes the loops listed
above independently of deletion: `EnvironmentJobWorkflow` ends when its
environment is closed or its universe is unknown, and
`SubagentExecutionWorkflow` ends when its universe is unknown.

**3. Environments.** Begin closing every environment that is not closed, with
the new close reason `universe_deleted`. This step is best effort and
bounded: it tells whoever can be told, and then deletion goes on.

| Kind | What closing does | What stays |
| --- | --- | --- |
| Provisioned | The row goes to `closing`; the existing reconciler sends `target/close` to the provider. A provider that is no longer registered closes the row without a call, as today. | Nothing if the provider answered. Otherwise the machine, which is then recorded (below). |
| Registered | The row closes at once and its routes are revoked. A connected daemon receives `registration/closed` and exits. A daemon that is not connected finds its registration key unknown when it next dials in, and exits then. | The machine, which belongs to whoever runs the daemon. |
| External | The row closes at once. | The endpoint, which Lightspeed never owned. |

The step ends when every provisioned environment is closed, or when each one
still closing has had two close attempts, or after two minutes, whichever
comes first. The time limit covers a deployment where no process holds the
environment reconciler's role. A machine whose provider did not confirm the
close is written into the deleted universe's record with its provider and
provider target id, so an operator can find and remove it. Nothing retries
after that: the rows that drove the reconciler are gone.

This fits how environments are mostly used where universes come and go:
developers run daemons in their own containers, which stop with those
containers and have their own life cycle.

Environment rows are not deleted one by one; there is no such operation and
none is added. They go with the universe. The control-connection watcher is
changed to treat a missing row as gone, so a daemon is disconnected even if
its row disappears some other way.

**4. Storage.** Delete the universe's objects by catalog, then clear its
object prefix. Object uploads hold a shared per-universe database lock through catalog
publication; storage takes its exclusive counterpart before sweeping. This
waits for pre-fence uploads without delaying the deletion request. The pass
verifies the prefix is empty before finalizing. Only then, in one transaction: record the
UUID as deleted and delete the `universes` row. A failure to clear the prefix
now keeps the universe in *deleting* and is retried, instead of being logged
after the row is gone.

### Deleted universes

A table of its own, `universes_deleted`, keeps one row per deleted universe
and is never pruned. It is the record of what went away. Each row holds:

- `universe_id`;
- the handle and display name the universe had when deletion was requested;
- `created_at_ms`, `deleting_at_ms` and `deleted_at_ms`;
- who requested the deletion, as an attribution;
- a `summary` document with what the universe contained and what deletion
  removed.

The summary is one JSON document, so a new kind of resource adds a field and
not a migration:

```json
{
  "contained": {
    "sessions": 412,
    "bots": 3,
    "workspaces": 9,
    "profiles": 6,
    "environments": { "provisioned": 1, "registered": 4, "external": 0 },
    "mcpServers": 2,
    "apiKeys": 5,
    "channelAccounts": 1,
    "blobBytes": 73400320,
    "lastActivityAtMs": 1794000000000
  },
  "removed": {
    "workflowsTerminated": 37,
    "schedulesDeleted": 2,
    "objectsDeleted": 1250,
    "environmentsClosed": 4,
    "environmentsUnconfirmed": [
      { "providerId": "incus-a", "providerTargetId": "ls-7f3e-12" }
    ]
  }
}
```

- `contained` is counted once, when deletion is requested, before anything is
  torn down. It holds counts and sizes only: no names, no content, no
  secrets.
- `removed` is what the pass did, added up as it goes. After an interrupted
  pass these counts can be low, because work done just before the
  interruption may not have been written down. They describe the teardown;
  `contained` is the figure to rely on.
- `environmentsUnconfirmed` lists the machines whose provider did not confirm
  the close.

While the universe is deleting, the same document lives on its `universes`
row; the last step moves it into the record.

The record serves three purposes:

- **A reference.** What existed, how much was in it, when it went, who
  removed it and what may still be running for it.
- **No resurrection.** Creating a universe with a recorded UUID is refused.
  A straggler that still holds the id cannot write into a new universe that
  happens to reuse it.
- **A definite answer.** Reading a recorded UUID fails with the distinct error
  kind `gone`, where an id that never existed is `notFound`. The error carries
  the record. The Platform needs that difference (below).

The record is not the `universes` row kept with a deleted mark. Handle reuse
would not prevent that, since the handle is already unique only among
universes that are not deleting. The reason is the cascade: every universe
table hangs off the `universes` row by foreign key, and deleting that row is
what removes everything and shows that nothing is left. Keeping the row would
mean deleting from each table by hand, and every table added later would have
to be remembered. It would also put deleted universes in front of every query
and pass that reads the table.

Because the record is permanent, a UUID is never used twice.

`deployment/universes/deleted/list` returns the records, newest first and
paged. It follows `deployment/sessions/deleted/list` and is in the
`deployment/universes` group.

This is a record of resources that existed, with the attribution other core
records carry. It is not a log of requests; the Platform's audit trail stays
the one place for that.

### Other processes

The change feed from [P202](p202-session-summaries-and-change-signals.md)
gains a universe-level change, published at the fence and again when the row
is deleted. On it, every process drops what it holds for the universe:

- the cached universe state, including its model discovery cache;
- the watch hub, which ends the universe's open watch streams;
- registered control connections, pending routes and revocation
  subscriptions;
- blob cache entries.

The feed is a signal, not the guarantee. The guarantee is the per-request
database check in the gateway and the 30-second re-validation in workers.

### API and CLI

- `deployment/universes/delete` returns `{universeId, state}` with `state`
  `deleting` or `deleted`. It is idempotent: a deleting universe returns
  `deleting`, a recorded one `deleted`, an id that never existed `notFound`.
  The counts in today's response (`workflowsTerminated`,
  `blobObjectsDeleted`) are removed; the work no longer happens in the
  request.
- `DeploymentUniverseView` gains an optional `deletion` object:
  `startedAtMs`, `requestedBy` (an attribution), `step` (`intake`,
  `workflows`, `environments` or `storage`) and an optional `detail` naming
  what the step is waiting for. `list` includes deleting universes.
- New error kind `gone`.
- New method `deployment/universes/deleted/list`.
- `lightspeed universe delete <universe> [--wait [DURATION]] [--yes]`. It
  asks for the handle to be typed unless `--yes` is given. `universe list`
  marks deleting universes, and `universe list --deleted` shows the records.

The API contract and every generated consumer are regenerated.

### Storage

One migration, schema revision 25, with `release/metadata.env` aligned:

- `universes.deleting_at_ms` and `universes.deletion_json`, both nullable.
  The document holds who requested the deletion, the step and its detail, and
  the summary being built.
- The unique constraint on `handle` becomes a partial unique index over
  universes that are not deleting.
- The `universes_deleted` table.
- `universe_deleted` added to the environment close reasons.

### Platform

- **Delete no longer requires archiving.** The archive requirement existed so
  that no traffic raced the deletion; the fence does that now. Archiving stays
  as an organizational state. The delete dialog asks for the handle to be
  typed.
- **The Platform follows the runtime.** Its periodic inventory sync marks a
  linked universe as deleting when the runtime reports it so, which hides it
  from everyone but the Platform admin page. A linked universe that is absent
  from the inventory is read by UUID: `gone` removes the Platform's rows
  (organization, memberships, setups); `notFound` leaves it as "missing", as
  today. A Platform pointed at the wrong or an empty runtime therefore never
  removes memberships.
- **Platform admin → Universes** shows deleting universes with their step and
  detail.
- **Create in engine** is refused for a deleted universe.
- The Platform's audit trail records the request as `universe.delete`.

## What deletion does not remove

These remain, and the user documentation will say so:

- **Temporal histories.** Terminated workflows keep their history, including
  payloads, until the namespace's retention period ends.
- **Machines Lightspeed does not own:** the hosts behind registered daemons
  and external endpoints.
- **Connector-local state,** such as the WhatsApp authentication directory a
  connector host keeps per account. See open decision 2.
- **State at third parties:** tokens issued by OAuth providers, usage at
  model providers, anything an agent did outside.
- **Backups** of the database and the object store.

## Slices

Each slice lands with its tests.

1. **Fence and record.** The migration; the three states; the fence in key
   and universe resolution; no row insert when building state; the deletion
   pass doing what today's request does (session workflows, objects, row) in
   the new order; the deleted-universes record with its summary, its list
   method and `gone`; passes skip
   deleting universes; `universe delete --wait`.
2. **Workflows.** Universe-prefixed workflow-tool execution ids. The intake
   step. Termination by prefix listing. The two self-ending workflows. The id
   prefix test.
3. **Environments.** Close on delete, the `universe_deleted` reason, the
   bounded wait, recording unconfirmed machines, the watcher fix.
4. **Other processes.** The universe-level change and what each process drops
   on it; worker re-validation.
5. **Platform.** Delete without archive, the deleting state, removal on
   `gone`, the audit entry, the demo backend. Rewrite "Archive and delete a
   universe" in `docs/documentation/deployment/multi-tenancy.md`, for review
   before it lands.
6. **Provider resources (optional, later).** Slices 1 to 5 need no protocol
   revision: `target/close` and `registration/closed` exist. What they leave
   is the per-universe resources a provider keeps besides machines (for
   Incus: the project, network, ACL and profile). Removing those needs one
   new controller method that releases everything a provider holds for a
   universe, which is an environment-protocol revision. The environment
   reconciler would call it for each provider bound to a deleting universe,
   inside the same bound as the machine closes and equally best effort. It
   is not needed while universes that come and go use registered daemons.

## Tests

The measure is a live suite, `universe_deletion_live`, that builds one
universe holding everything and asserts that nothing is left.

**The universe under test contains:** a session with a run in flight; closed
and soft-deleted sessions; a sub-agent execution, a code execution and an
environment job in flight; a bot with a schedule trigger and a running
controller; a channel account with an open conversation workflow; a
registered environment with a connected daemon; a provisioned environment
through the fake controller; an external environment; a workspace with
objects in the object store; universe keys, registration keys, MCP servers,
credentials, profiles and model defaults; an open watch stream.

**Immediately after the call:**

- a universe key is `unauthenticated` and a deployment key gets `notFound`;
- a new universe can take the handle;
- `read` reports `deleting`.

**Eventually:**

- Temporal has no open execution and no Schedule with the universe's prefix;
- the fake controller received `target/close` for every machine;
- the daemon received `registration/closed`;
- the universe's object prefix is empty;
- no table with a `universe_id` column holds a row for it. The test finds
  those tables from the database catalog, so a table added later is covered
  without touching the test;
- `read` answers `gone`, and creating the same UUID is refused;
- the record's `contained` counts match what the test built, and
  `deployment/universes/deleted/list` returns it.

**Further cases:**

- **Second process.** Another runtime process with the universe's state
  cached stops serving it within the bound.
- **Interruption.** The pass is stopped after each step in turn and started
  again; every run converges.
- **Unreachable provider.** The universe is deleted within the bound, and its
  record names the machine that was not confirmed closed.
- **Disconnected daemon.** The universe is deleted; the daemon is refused
  when it dials in again.
- **Loop.** Fifty universes are created, given a session each, and deleted;
  the residue checks above pass for all of them.
- **Platform.** A universe deleted through the runtime disappears from the
  Platform; a universe missing for another reason does not.

Unit tests cover the state transitions in the store, the order of the pass
against fakes, and the id prefix.

## Decisions

Decided, 2026-11-16:

- **A table of deleted universes** as a reference, separate from the
  `universes` table, named `universes_deleted`.
- **The record is kept indefinitely** and says what was in the universe and
  what was removed, without being more than counts and a few ids.
- **Environments never block deletion.** Providers and daemons are told on a
  best-effort basis, with a couple of attempts, and deletion goes on.
- **Workflows are found by workflow id prefix** and terminated.
- **A protocol revision is acceptable where one is needed.** Only slice 6
  needs one.

- **Deletion is asynchronous.** The call sets the fence and returns; a worker
  pass does the teardown; the CLI can wait for it to finish. The work has no
  fixed length (one Temporal call per workflow, two listings some seconds
  apart, up to two minutes for environments), a request that dies halfway
  would leave a half-deleted universe, and expiry has no caller at all.
- **Temporal's prefix listing is assumed.** The Temporal documentation lists
  `STARTS_WITH` on `WorkflowId`, and the Rust client has `list_workflows`. No
  fallback is built. The live suite asserts that no execution with the
  universe's prefix is left, so a server that lacks the operator fails there
  and not silently.

Implementation scope:

1. **Workflow-tool executions in flight at the upgrade** keep their old ids.
   The greenfield implementation uses the new composition without migration;
   upgrades must follow the existing workflow-compatibility release procedure.
2. **Connector-local state** remains outside this teardown. A connector host
   could remove an account's local directory when the account is absent from
   a listing that includes disabled accounts.

## Validation and progress

- 2026-11-16: design written from a survey of the delete path, the workflow
  inventory, environment teardown and per-process state. Revised the same
  day: environment teardown is best effort and bounded; the deleted
  universe's record is permanent and summarizes what the universe contained
  and what was removed. No code changed.

- 2026-11-16: implemented schema revision 25, permanent deleted-universe
  records, asynchronous admission fencing and resumable worker teardown;
  universe-prefixed workflow-tool IDs and terminal unknown-universe behavior;
  bounded environment closure; cross-process invalidation and worker cache
  revalidation; CLI deletion/wait and deleted-record listing; Platform
  reconciliation, progress display and handle confirmation. Regenerated public
  API/workflow contracts and TypeScript/Python consumers. Added upload and
  schedule guards for work admitted before the fence. The deployment guide
  was updated with permission for review.
- Rust validation passed: API unit and schema freshness tests; workflow units,
  contract freshness and all workflow-ID composers; runtime units (659 passed,
  one ignored); environments (44 passed); store units and the isolated live
  deletion test; CLI units (235 passed). The exact workspace/all-targets Clippy
  command passed with warnings denied. The live upload regression verifies that
  storage waits for a pre-fence upload and cannot admit another afterward.
- TypeScript type checks, production/demo builds, SDK tests (109), Python
  generation freshness, mypy and tests (46 passed, one opt-in live test skipped),
  documentation publication/link checks and release metadata verification
  passed. Full Platform suites encountered timing failures under concurrent
  builds; their isolated reruns passed.
- Runtime live validation: the five new cases passed, including rich resource
  inventory teardown across restarted runtimes, a second runtime's cache,
  fifty universes with live workflow executions, provider closure and bounded
  failure, connected/disconnected daemons, watch-stream closure, and real
  workflow self-termination. The self-termination case exposed a local-activity
  failure-wrapper difference; recognizing both wrapper forms fixed it, with
  regression coverage. The final combined deletion suite passed all five cases.
  Another 37 serialized live checks passed across deployment universes, tenancy,
  provisioned and registered environments, workflow-tool plugins, subagents and
  code-workflow start/cancellation. Live checks used local PostgreSQL, Temporal
  and MinIO; no slow production-budget suite was run. Local migration from
  revision 24 to 25 passed. Optional provider-wide auxiliary-resource release
  and connector-local directory cleanup remain deferred as described above.
