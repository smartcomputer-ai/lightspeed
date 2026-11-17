# Operate Lightspeed

Operating Lightspeed means observing both the services and the work moving
through them. An HTTP listener can be healthy while a session has no worker,
a model credential is unusable, or a machine is offline. Keep a small
verification task that exercises the capabilities your installation actually
provides, alongside process and infrastructure monitoring.

After a deployment change, sign in, read a known workspace file, and complete
a short session run. If the installation supplies compute, check that path too. [Troubleshooting](troubleshooting.md) follows failures
across the services; [upgrades and recovery](upgrades-and-recovery.md)
covers planned maintenance and restored state.

## Know what health checks establish

| Component | Endpoint | Meaning |
| --- | --- | --- |
| Runtime process with `gateway` or `environment-gateway` | `GET /health` | Returns `ok` once HTTP is serving. It does not continuously check dependencies or worker readiness. |
| Runtime worker-only process | No HTTP health endpoint | Observe the process and its Temporal pollers, task progress, and failures. |
| Platform | `GET /health` | Returns `{"ok":true}`. Startup migrations precede serving, but the endpoint does not recheck the database or runtime. |
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

## Find the responsible process and workflow

Start with the universe UUID and the affected session, run, bot, or
environment ID. Record the error time and deployed release. These
identifiers connect the user-visible problem to logs and workflow history.

Inspect the single-host services with:

```bash
docker logs --tail 100 lightspeed-runtime
docker logs --tail 100 lightspeed-platform
```

The runtime supports `compact`, `pretty`, and `json` through
`LIGHTSPEED_LOG_FORMAT`. JSON is useful when collecting logs from several
roles. `RUST_LOG` controls detail; for a focused investigation, a filter such
as `warn,temporal_runtime=debug,temporal_workflow=info` adds runtime detail.
Restart the affected process to apply an environment-file change.

Session logs can include `universe_id`, `session_id`, `workflow_id`,
`temporal_run_id`, `lightspeed_run_id`, and `session_head_seq`. A session's
Temporal workflow ID is `<universe-uuid>/<session-id>`. Use the runtime UUID,
which is visible in the universe's **Settings → General → Identifiers**, rather than the
handle in the Platform URL.

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
shows active work on a workflow that has stopped. The promise reaper checks a fresh
read before reporting it, and it does not restart anything. Bring the session back with
[`session/restart`](upgrades-and-recovery.md#restart-a-stopped-session), and preserve
the evidence before using destructive Temporal operations. A force close is permanent.

`promise_repair_repeated` means a repair signalled on the previous pass, five minutes
earlier, has still not landed in the log. The workflow may be stalled, or busy. Look at
its pending workflow task in the Temporal UI next. A stalled workflow with no due
promise and no overdue timer is not reported by anything here, since the runtime exports
no SDK metrics. Every reaper pass logs a summary with `duration_ms`, `sessions_scanned`,
`events_read` and its repair counts. Each `sessions` replica makes its own pass.

### Metrics and diagnostic payloads

Use application logs and the infrastructure's metrics for routine
monitoring.

`LIGHTSPEED_LLM_DEBUG_DUMPS=true` stores raw provider request and response
payloads as unreferenced CAS blobs and logs their references at debug level.
Credentials are redacted, but requests include the conversation context.
Include `llm_runtime=debug` in `RUST_LOG` to see the dump references; for example,
`warn,temporal_runtime=debug,temporal_workflow=info,llm_runtime=debug`. Enable
this for a focused investigation, restrict access to the collected material,
and disable it afterward. These dumps are subject to blob collection; they
are not a permanent audit archive.

## Scale the process that does the work

Run the [runtime roles](overview.md#runtime-roles-and-scaling) together at
first. Select a subset with `LIGHTSPEED_ROLES` or `--roles` when you want
to place or scale it separately. Keep storage, secrets, Temporal, and
environment routing aligned across processes. Each worker replica polls
its role's task queue.

Consider pending work, activity duration, provider rate limits, and storage
load before adding workers. Each process has its own blob cache, so budget
memory per replica. See [Configure a deployment](configuration.md).

To run environment gateways as replicas, give each a reachable
`LIGHTSPEED_ENVIRONMENT_GATEWAY_PEER_URL`. Configure one shared
`LIGHTSPEED_ENVIRONMENT_GATEWAY_URL` for workers and the same
`LIGHTSPEED_ENVIRONMENT_GATEWAY_TOKEN` on workers and gateways. During
shutdown, readiness drops and existing streams have a short drain window.
Daemons reconnect through the shared address. Confirm what completed before
retrying an interrupted operation. See
[Networking and ingress](../environments/networking-and-ingress.md).

### Partition connector accounts

When you run more than one connector host, assign each account to only
one host with `LIGHTSPEED_CONNECTOR_ACCOUNTS`. Preserve WhatsApp's
authentication directory when moving an account, and stop the old runner
before starting the new one. Check the new host's `/healthz` inventory
and `/readyz` before sending a test message. See
[Channel connectors](../integrating-and-extending/channel-connectors.md).

## Manage retention and blob collection

Closing a session preserves its history. A universe Owner can set a
soft-deletion delay for a root session. Automatic deletion waits until the
delay has elapsed and all retained descendants are closed. Soft deletion
hides sessions but keeps their storage. A Platform Admin can purge retained
history from the universe's **Settings → General → Deleted sessions**. See
[Sessions and runs](../using-lightspeed/sessions-and-runs.md#close-and-retain-a-session).

The `sessions` role collects blobs with no durable references after a grace
period. The default is seven days since the last write or API admission of
a reference. A read does not extend it. Set the grace period in
`LIGHTSPEED_CAS_SWEEP_GRACE_MS` to cover expected upload-to-use and workflow
handoff delays. A value of zero disables background collection.

To inspect a sweep without deleting content, run it with the deployment's
runtime configuration.

```bash
docker run --rm --network lightspeed --env-file runtime.env \
  "lightspeed-runtime:$LIGHTSPEED_RELEASE_ID" cas-sweep --dry-run
```

A pass is bounded. Review candidate and error counts, and check object delete
failures separately from database errors. Run without `--dry-run` to remove
eligible content. A manual sweep yields to an active background leader.

For deleting a whole universe, see
[Managing universes](multi-tenancy.md#archive-and-delete-a-universe).

## Stop and restart services

Use the service manager's normal shutdown path. The runtime and Platform
handle SIGINT and SIGTERM. The self-hosting container allows 30 seconds
for runtime shutdown. Check the outcome of any in-flight external
operation before retrying it.

Restarting a worker is distinct from canceling a run, closing a session, or
deleting data. With compatible code and retained Temporal/store state, workers
can resume durable workflow processing. Preserve the databases, object data,
keys, and optional local state that those workflows use. A release that changes
workflow code needs [a new namespace](upgrades-and-recovery.md#decide-whether-the-release-breaks-open-work)
rather than a rolling restart.

Closing a session waits at most 45 seconds for the workflow. A plain close that is
held, for example behind a compaction, answers `operationPending` after that. A force
close terminates a workflow it has not heard from in that time, closes the log itself,
and sends terminal notifications to the session's controller.

For planned maintenance, inspect active runs and jobs and control incoming
work before stopping processes. Follow the
[upgrade procedure](upgrades-and-recovery.md#upgrade-the-single-host-installation)
when changing releases. For local development, `./dev.sh status` reports the
supervisor's state; routine diagnosis does not require resetting its storage.
