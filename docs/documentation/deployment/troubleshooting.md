# Troubleshoot a deployment

Start with the last step that worked. If sign-in succeeds but a universe
will not open, investigate the Platform's runtime access. If a run is accepted
but does not progress, inspect its state and workers. This narrows the problem
without repeatedly restarting services that are already doing their job.

Record the deployed release, time, exact error, universe UUID, and the affected
session/run, bot, environment, or channel account IDs. Capture the relevant
logs before changing state. Keep credential values and private conversation
content out of reports unless they are necessary and the report's recipients
are authorized to see them.

<a id="locate-the-failing-boundary"></a>

## Find where the request fails

| Symptom | Start here |
| --- | --- |
| Runtime or Platform does not start | [Startup and configuration](#startup-and-configuration) |
| Sign-in or universe access fails | [Authentication and universe access](#authentication-and-universe-access) |
| A run is queued, waiting, or apparently stuck | [Session progress](#session-progress) |
| The model rejects a call or tools are absent | [Models and MCP](#models-and-mcp) |
| A machine is offline or a command cannot see a file | [Execution environments](#execution-environments) |
| A larger upload or stored payload fails | [Blob writes](#blob-writes) |
| Chat messages do not reach the agent or return | [Chat delivery](#chat-delivery) |
| Closing work does not reclaim storage or compute | [Retention and cleanup](#retention-and-cleanup) |

The [operations guide](operations.md) lists health endpoints and their limits.
A runtime `/health` response proves the listener is serving; it does not check
Temporal pollers, model access, or useful session progress.

## Startup and configuration

For the self-hosting containers, inspect the process output:

```bash
docker logs --tail 100 lightspeed-runtime
docker logs --tail 100 lightspeed-platform
```

If the runtime exits before serving, check the error against these dependencies:

| Error area | What to inspect |
| --- | --- |
| PostgreSQL connection | The runtime database URL, credentials, network path, and required connection options. Container-local `localhost` is not the database host. |
| Schema verification | Use the deployed release's `schema-version` diagnostic. The runtime never migrates implicitly; follow the explicit migration procedure. |
| Temporal connection | The frontend address and an existing namespace, reachable from the process. |
| Object-store configuration | A nonempty bucket when any `LIGHTSPEED_OBJECT_STORE_*` variable is set, plus the intended endpoint and credentials. |
| Role or environment routing | Valid roles and the internal environment gateway URL/token on every process without that role. |
| Secret configuration | A base64 master key decoding to 32 bytes, matching the stored encrypted state. |

The Platform needs its own database and authentication settings. Its migrations
run before HTTP serving. Do not point it at the runtime database or reset
either database to work around a startup error. Use
[Configuration](configuration.md) for settings and
[Upgrades and recovery](upgrades-and-recovery.md) for ledger failures.

For local development, `./dev.sh status` reports the supervised stack. Check
the relevant service's logs and prerequisites before restarting it.

## Authentication and universe access

If sign-in fails, compare the browser's actual origin with
`LIGHTSPEED_PLATFORM_BASE_URL` and any configured trusted origins. Check HTTPS
termination and forwarded host/scheme handling. Retain the Platform's
authentication secret across restarts.

The administrator bootstrap variables apply only while the users table is
empty. They cannot reset an existing password. Use **Platform admin → Users**
from an authorized administrator account for account management.

When sign-in works but a page fails, distinguish a Platform permission denial
from an upstream runtime error. Viewers can read visible work, Contributors can
run work, Operators can configure shared resources, and Admins manage membership
and keys. Admins can access every session; other members see their own private
sessions and sessions shared with the universe. A private session outside that
audience appears absent. See [People and roles](../access-and-security/people-and-roles.md)
for the permission matrix.

For runtime errors, verify the endpoint, key status, key scope, and allowed
method groups. The Platform needs a deployment key allowed to assert actors.
It sends the runtime universe UUID, which differs from the browser slug.
A direct universe key must omit `x-lightspeed-universe`; a deployment key must
send it for universe methods and omit it for deployment methods. See
[API keys and service access](../access-and-security/api-keys-and-service-access.md)
for request authentication, and [People and roles](../access-and-security/people-and-roles.md)
for membership changes and offboarding.

## Session progress

Open the session and inspect the current run and pending work before assuming
a worker failure. A later submission can queue behind an active run. A run
waiting for tool approvals, a timer, an environment job, or a sub-agent can
also be behaving as intended. Complete all required approval decisions and
inspect the source it is waiting for.

If there is no expected wait, inspect Temporal using the correct namespace
and workflow ID, `<universe-uuid>/<session-id>`. Check workflow status, pending
activities, failures, and pollers on the configured session queue. Workflow
and activity polling must both be available. A process serving the `bots`
queue does not substitute for a missing `sessions` worker.

Use the Temporal execution chain when a session has continued as new. The
Temporal run ID differs from the Lightspeed run ID shown for one agent task.
[Operations](operations.md#find-the-responsible-process-and-workflow) explains
the log identifiers.

If Temporal says the workflow is terminal or missing while the session still
shows an active run, look for `session_workflow_failed`,
`stale_active_projection`, or `session_bootstrap_failed`. The maintenance
reaper does not automatically resume an arbitrary failed workflow. Preserve
the history and error, then follow [recovery guidance](upgrades-and-recovery.md#recover-a-failed-upgrade).
Terminating, resetting, or force-closing work changes its lifecycle; it is not
an ordinary retry of a read request.

See [Sessions and runs](../using-lightspeed/sessions-and-runs.md) for queueing,
steering, cancellation, and closure behavior.

## Models and MCP

For a model failure, inspect the selected provider ID, API kind, and model
name together. Verify the integration in the same universe as the session.
Then check the provider's returned error: authentication, quota, unsupported
parameters, and network failures require different corrections.

A disabled or unusable universe provider record blocks a
deployment fallback key. Removing the record permits fallback again, so do
not confuse removal with disabling access. A coding-agent subscription login
also does not authenticate Lightspeed's own session inference. Follow
[Models and credentials](../using-lightspeed/models-and-credentials.md).

If discovery succeeds but an agent has no expected tool, inspect its profile's
capability grants and the MCP server/tool selection. Registering a server
does not grant every session permission to use it. Also distinguish a tool
waiting for approval from one that was never exposed.

For private MCP endpoints, native discovery and execution require both the
server's private-network opt-in and the runtime's host/CIDR allowlist. OAuth
network permission is separate. Provider-hosted execution requires the model
provider to reach the MCP endpoint, which is a different network path from
the runtime reaching it. [Tools and MCP](../using-lightspeed/tools-and-mcp.md)
explains execution modes and discovery behavior.

## Execution environments

Begin with the environment's source, lifecycle, and power state. A paused or
stopped VM needs a wake path; an offline registered machine needs its daemon;
a provisioning failure needs the provider and template that created it.

For a registered daemon, inspect its own output and both public WebSocket
paths. The control connection can be established while data routing is still
broken. Verify the deployment has one environment gateway, that the proxy
preserves upgrades, and that runtime processes use the correct internal URL
and token. Retain the daemon state directory so a restart proves the same
identity. A closed identity cannot reconnect as a new environment merely by
reusing its key.

For Incus, inspect provider `/health`, the universe provider binding, selected
template, instance boot, and guest daemon. Follow the specific procedures in
[Bring your own compute](../environments/bring-your-own-compute.md),
[Incus VMs](../environments/incus-vms.md), and
[Networking and ingress](../environments/networking-and-ingress.md).

If a process cannot see a workspace file, check which filesystem contains it.
VFS workspace files do not automatically appear on an execution environment's
disk. Transfer required inputs explicitly and verify the process working
directory. [Processes and jobs](../environments/processes-and-jobs.md) also
explains why a daemon restart interrupts persisted active jobs and loses
interactive process handles.

## Chat delivery

Follow delivery in three stages: connector account, runtime routing, and
outbound response.

1. Inspect the connector host's `/healthz` and expected account inventory.
   Verify its provider/account selection, runtime API access, Temporal
   namespace, and current provider login. Platform's **Waiting for connector** display can
   mean health aggregation is missing; check
   `LIGHTSPEED_PLATFORM_CHANNELS_HEALTH_URLS` before concluding the host is down.
2. If the account is ready but no agent runs, inspect the chat trigger,
   pairing, sender allowlist, group activation policy, and paused bot/trigger
   state. Connector readiness does not establish that a message has a route
   to a bot.
3. If the agent produced a response but chat did not receive it, inspect the
   delivery activity and the connector's per-account activity worker. Check
   provider authentication and WhatsApp linked-device state where applicable.

Also check discovery freshness: `/readyz` can remain successful after a later
discovery failure, and a host with no selected accounts can be ready. Duplicate
hosts serving the same account can conflict, particularly when polling one
Telegram token. Use nonoverlapping account ownership as described in
[operations](operations.md#partition-connector-accounts).

[Chat channels](../using-lightspeed/chat-channels.md) provides the complete
account, pairing, and routing setup.

## Blob writes

If the runtime starts successfully but a larger file, attachment, model
payload, or diagnostic dump fails with
`blob exceeds inline threshold (65536 bytes) but no object store is configured`,
configure S3-compatible storage on the runtime processes using CAS. The current
PostgreSQL-only setup accepts blobs up to 64 KiB; larger blobs require that
backend. Follow [Choose the blob backend](configuration.md#choose-the-blob-backend)
and retry a controlled write after restarting the affected processes.

When object storage is already configured, distinguish a failed write from
a missing existing object. Check its endpoint and permissions for writes;
for reads, also check the exact physical location retained by the blob catalog.
A bucket or endpoint change does not copy the old bytes.

## Retention and cleanup

Closing a session retains history unless a deletion policy later removes it.
Automatic deletion waits for the root's configured deadline and for its
retained descendant tree to be closed. Blob collection then follows durable
references and its own grace period; repeated reads do not renew that period.

A machine's lifecycle is separate. Inspect environment ownership, retention,
and provider cleanup before expecting a session close to destroy a VM. A
bring-your-own machine remains under its operator's control. The
[retention guide](operations.md#manage-retention-and-blob-collection) and
[environment cleanup guide](../environments/power-and-cleanup.md) explain the
individual decisions.

Universe archive is also separate from shutdown. It does not stop automation
or revoke access, and permanent purge is not a complete infrastructure cleanup
operation. Follow [Managing universes](multi-tenancy.md#archive-and-delete-a-universe)
when retiring an entire tenant.
