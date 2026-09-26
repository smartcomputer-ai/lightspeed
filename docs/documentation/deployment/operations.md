# Operate Lightspeed

Operating Lightspeed means observing both the services and the work moving
through them. An HTTP listener can be healthy while a session has no worker,
a model credential is unusable, or a machine is offline. Keep a small
verification task that exercises the capabilities your installation actually
provides, alongside process and infrastructure monitoring.

After a deployment change, sign in, read a known workspace file, and complete
a short session run. If the installation supplies compute or chat channels,
check those paths too. [Troubleshooting](troubleshooting.md) follows failures
across the services; [upgrades and recovery](upgrades-and-recovery.md)
covers planned maintenance and restored state.

## Know what health checks establish

The components expose different observations:

| Component | Endpoint | Meaning |
| --- | --- | --- |
| Runtime process with `gateway` or `environment-gateway` | `GET /health` | Returns `ok` once HTTP is serving. It does not continuously check dependencies or worker readiness. |
| Runtime worker-only process | No HTTP health endpoint | Observe the process and its Temporal pollers, task progress, and failures. |
| Platform | `GET /health` | Returns `{"ok":true}`. Startup migrations precede serving, but the endpoint does not recheck the database or runtime. |
| Connector host | `GET /healthz` | Reports discovery and served-account state, including degraded state, with HTTP 200. |
| Connector host | `GET /readyz` | Returns 200 after at least one successful discovery and while every currently served account is ready; otherwise 503. |
| Incus provider | `GET /health` | Checks the active Incus topology and returns topology information or HTTP 503. |
| Environment daemon | No HTTP health endpoint | Observe daemon output and environment readiness, then test a harmless filesystem or process operation. Its passive listener speaks WebSocket. |

For the [single-host installation](self-hosting.md), the listener checks are:

```bash
curl --fail http://127.0.0.1:18080/health
curl --fail http://127.0.0.1:3000/health
```

Query optional services at their configured internal addresses. Their ports
are not automatically published by the self-hosting recipe. Keep operational
listeners and the Temporal management interface on the intended private
network.

Connector readiness needs context. A host with zero selected accounts can be
ready, and readiness has no discovery-freshness deadline. After an initial
successful pass, later discovery failures may leave it ready if its existing
accounts remain ready. Monitor `discovery.lastSuccessAtMs`, `lastError`, and
the expected account inventory in `/healthz` as well as `/readyz`.

## Find the responsible process and workflow

Start with the universe UUID and the affected session, run, bot, environment,
or channel account ID. Record the error time and deployed release. These
identifiers connect the user-visible problem to logs and workflow history.

Inspect the single-host services with:

```bash
docker logs --tail 100 lightspeed-runtime
docker logs --tail 100 lightspeed-platform
```

The runtime supports `compact`, `pretty`, and `json` through
`LIGHTSPEED_LOG_FORMAT`. JSON is useful when collecting logs from several
roles. `RUST_LOG` controls detail; for a focused investigation, a filter such
as `warn,temporal_server=debug,temporal_workflow=info` adds runtime detail.
Restart the affected process to apply an environment-file change.

Session logs can include `universe_id`, `session_id`, `workflow_id`,
`temporal_run_id`, `lightspeed_run_id`, and `session_head_seq`. A session's
Temporal workflow ID is `<universe-uuid>/<session-id>`. Use the runtime UUID,
which is visible in **Settings → General → Identifiers**, rather than the
Platform URL slug.

The Temporal execution ID and Lightspeed run ID identify different things.
Temporal continue-as-new starts another execution of the same logical session
workflow; the session and its current Lightspeed run can continue across that
transition. Search the workflow's execution chain when investigating an event
that predates the current execution.

In the deployment's Temporal UI or administrative tooling, inspect the
namespace, workflow status, pending activity failures, and task-queue pollers.
The development stack exposes its UI at `http://localhost:8233`; a deployed
installation uses the interface configured for its Temporal service.

`session_workflow_failed` and `session_rollover_delayed` identify failures and
delayed history rollover. `stale_active_projection` means stored session state
still shows active work while Temporal reports a terminal or missing workflow.
The promise reaper observes this condition; it does not resurrect an arbitrary
failed workflow. Preserve the evidence and follow the recovery guidance before
using destructive Temporal operations.

### Metrics and diagnostic payloads

The connector host exposes Prometheus metrics at `/metrics` on its health
listener, port `8090` by default. These cover discovery, readiness, reconnects,
and inbound admission. Its Temporal SDK exporter is a separate listener,
defaulting to port `9090`. Configure collection for both if those signals are
needed. The Rust runtime and Platform do not currently expose an application
Prometheus endpoint; use their logs and the infrastructure's own monitoring.

`LIGHTSPEED_LLM_DEBUG_DUMPS=true` stores raw provider request and response
payloads as unreferenced CAS blobs and logs their references at debug level.
Credentials are redacted, but requests include the conversation context.
Include `llm_runtime=debug` in `RUST_LOG` to see the dump references; for example,
`warn,temporal_server=debug,temporal_workflow=info,llm_runtime=debug`. Enable
this for a focused investigation, restrict access to the collected material,
and disable it afterward. These dumps are subject to blob collection; they
are not a permanent audit archive.

## Scale the process that does the work

The runtime binary supports five roles:

| Role | Work it owns |
| --- | --- |
| `gateway` | JSON-RPC requests, OAuth callbacks, and bot webhook ingress. |
| `environment-gateway` | Daemon connections, worker environment routes, environment lifecycle reconciliation, and idle power management. |
| `sessions` | Session, sub-agent, and environment-job workflows and activities, plus session/promise maintenance and CAS collection. |
| `bots` | Bot controllers, trigger workflows and activities, and schedule reconciliation. |
| `channels` | Conversation workflows and core channel activities. |

Select roles with `LIGHTSPEED_ROLES` or `--roles`. Keep **exactly one active
environment-gateway process per deployment**. Its live daemon connections are
process-local. Replicating the default process, which includes every role,
would also replicate that role and is not a supported scaling recipe.

Split the roles before adding gateway or worker replicas. All processes must
agree on stores, secrets, namespace, task queues, and environment routing.
Every process without `environment-gateway` needs its internal URL and token.
[Configuration](configuration.md) describes that shared setup.

Workers can also separate workflow and activity polling with
`LIGHTSPEED_WORKER_TASK_TYPES=workflows` or `activities`, or the matching
`--task-types` argument. Local activities stay with workflow execution. Keep
both required types of pollers available on each subsystem's queue; a workflow
poller alone cannot execute remote activities.

Use pending tasks, activity duration/failures, provider limits, and storage
load to decide which capacity is missing. Adding session workers does not
increase a provider account's quota. Each process also has its own
`LIGHTSPEED_BLOB_CACHE_BYTES` budget, defaulting to 256 MiB, so account for
aggregate memory when adding replicas.

### Partition connector accounts

Connector hosts discover all enabled accounts for their selected providers
by default. They do not coordinate account ownership across hosts. Two
identically configured replicas can both consume the same account's updates.

Partition providers or set nonoverlapping
`LIGHTSPEED_CONNECTOR_ACCOUNTS=<universe-id>/<account-id>,…` selections. Keep
one update consumer for each Telegram token. For WhatsApp, retain the account's
authentication directory when moving it and stop the old host before starting
another owner. Verify the new host's discovered and ready account inventory.

## Manage retention and blob collection

Session closure, session deletion, blob collection, Temporal history
retention, and machine cleanup are separate operations. Closing a session
keeps its history. If `deleteAfterCloseMs` is configured on the session or
profile, the root session owns the later deletion deadline; otherwise there
is no automatic deletion deadline.

Forked and delegated descendants share that retained root. Automatic deletion
waits until the deadline and until the entire retained tree is closed. An
open descendant therefore prevents deletion of the tree. The session
retention reaper checks every five minutes and reports due roots, deletions,
open-tree skips, conflicts, and errors.

Blob collection runs separately. One elected `sessions` process examines the
CAS catalog hourly for old blobs without durable holders. The default grace
is seven days since the last put or API admission of the reference. Reading a
blob does not refresh that time. Committed durable references protect content;
profiles and uncommitted workflow handoffs do not hold blobs indefinitely.
Choose a grace period that covers expected upload-to-use and workflow handoff
delays.

Inspect one collection pass without deleting anything, using the same runtime
configuration as the deployment:

```bash
docker run --rm --network lightspeed --env-file runtime.env \
  "lightspeed-runtime:$LIGHTSPEED_RELEASE_ID" cas-sweep --dry-run
```

This continues the self-hosting recipe: run it from the deployment directory
with `LIGHTSPEED_RELEASE_ID` set to the deployed image's revision-specific tag.
The standalone binary equivalent is `lightspeed-server cas-sweep --dry-run`.

The report includes `rows_scanned`, `candidates`, `rows_deleted`, `bytes_freed`,
`objects_deleted`, and errors/conflicts. A pass is bounded, so its candidate
count is not a complete inventory of all reclaimable storage. Current passes
examine at most 100,000 rows per universe in pages of up to 1,024, with a soft
ten-minute deadline; the background leader retains a cursor for later passes.

Removing `--dry-run` requests deletion of eligible content. A manual deleting
pass yields to an active background leader and can report `leader_busy: true`.
`LIGHTSPEED_CAS_SWEEP_GRACE_MS=0` disables background collection; an explicit
sweep also requires a positive configured grace period.

Monitor object deletion failures as well as database results. Session deletion
does not set Temporal namespace history retention, remove separately retained
workspaces, erase connector authentication files, or guarantee machine
destruction. See [Sessions and runs](../using-lightspeed/sessions-and-runs.md)
and [Power and cleanup](../environments/power-and-cleanup.md) for their lifecycle
controls.

## Stop and restart services

Use the service manager's normal shutdown path. The current Rust runtime
handles Ctrl-C/SIGINT, which is why the self-hosting container uses
`--stop-signal SIGINT --stop-timeout 30`. Platform and connector hosts handle
both SIGINT and SIGTERM. Worker shutdown is bounded; stopping the process is
not proof that every long-running external operation completed.

Restarting a worker is distinct from canceling a run, closing a session, or
deleting data. With compatible code and retained Temporal/store state, workers
can resume durable workflow processing. Preserve the databases, object data,
keys, and optional local state that those workflows use.

For planned maintenance, inspect active runs and jobs and control incoming
work before stopping processes. Follow the
[upgrade procedure](upgrades-and-recovery.md#upgrade-the-single-host-installation)
when changing releases. For local development, `./dev.sh status` reports the
supervisor's state; routine diagnosis does not require resetting its storage.
