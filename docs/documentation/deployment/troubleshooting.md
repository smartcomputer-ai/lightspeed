# Troubleshoot a deployment

Start with the last step that worked. If sign-in succeeds but a universe
will not open, investigate the Platform's runtime access. If a run is accepted
but does not progress, inspect its state and workers. This narrows the problem
without repeatedly restarting services that are already doing their job.

Record the deployed release, time, exact error, universe UUID, and the affected
session/run, bot, or environment IDs. Capture the relevant
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
| A machine is disconnected or a command cannot see a file | [Execution environments](#execution-environments) |
| A larger upload or stored payload fails | [Blob writes](#blob-writes) |
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
| PostgreSQL connection | Check the database URL, credentials, and network path from the process. For Entra authentication, check its [workload identity configuration](configuration.md#postgresql-workload-identity). |
| Schema verification | Run the deployed release's `schema-version` diagnostic. Check the [migration baseline](upgrades-and-recovery.md#check-compatibility-before-reusing-state) before running the migrator. |
| Temporal connection | Check the frontend address, namespace, certificate trust, and credentials. See [Secure the Temporal connection](configuration.md#secure-the-temporal-connection). |
| Object-store configuration | Check that the selected backend has its complete set of settings, including credentials and the required bucket or container. See [Choose the blob backend](configuration.md#choose-the-blob-backend). |
| Role or environment routing | Valid roles and the internal environment gateway URL/token on every process without that role. |
| Secret configuration | A base64 master key decoding to 32 bytes, matching the stored encrypted state. |
| Retired model variables | Remove `LIGHTSPEED_CHAT_PROVIDER` and `LIGHTSPEED_CHAT_MODEL`; configure each universe's model defaults through Models or `lightspeed model defaults`. |

The Platform needs its own database and authentication settings. Its migrations
run before HTTP serving. Do not point it at the runtime database or reset
either database to work around a startup error. Use
[Configuration](configuration.md) for settings and
[Upgrades and recovery](upgrades-and-recovery.md) for ledger failures.

For local development, `./dev.sh status` reports the supervised stack. Check
the relevant service's logs and prerequisites before restarting it.

## Authentication and universe access

| Where it fails | What to check |
| --- | --- |
| Browser sign-in | Match `LIGHTSPEED_PLATFORM_BASE_URL` to the browser origin and verify HTTPS and forwarded host/scheme. For a company account, check provider admission and the configured groups. |
| Administrator bootstrap | Set the email and password before the first startup with an empty users table. To manage an existing user, use **Admin → Platform → Users**. |
| Platform API returns 401 | Check that the client is using the Platform URL and a valid session or Platform API key. Check key expiry, revocation, and whether the person or service user is still admitted. |
| Platform denies an operation | Check the caller's organization membership, universe grants, and any restrictions on the key. For a session, check its sharing and creator. A private session can appear missing to other members. |
| Runtime API returns 401 | Check the configured runtime token and the endpoint. For calls forwarded by Platform, compare `LIGHTSPEED_PLATFORM_API_TOKEN` or its `_FILE` value to the runtime configuration. |

For sign-in and offboarding, see [Company sign-in (SSO)](../access-and-security/single-sign-on.md)
and [People and roles](../access-and-security/people-and-roles.md). For token
installation and rotation, see
[API keys and service access](../access-and-security/api-keys-and-service-access.md).

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

If Temporal says the workflow is terminal or missing while the session is open,
`session/read` reports `executionFailure`, run starts answer
`sessionExecutionStopped`, and the session page shows a banner. Look for
`session_workflow_failed` or `stale_active_projection` in the logs. Nothing restarts
the session by itself. Preserve the history and error, then
[restart it](upgrades-and-recovery.md#restart-a-stopped-session), which fails a run that
was executing and keeps a parked one. A session that never finished setup answers
`sessionBootstrapFailed` instead. It has no run history to restart from, so force close
it and create a new one. A force close is permanent, so don't reach for it when you
still want the session back.

If the workflow shows as running but its tasks keep failing with a nondeterminism
error, or a panic saying an activity payload does not convert, the deployed release
cannot replay its history. See
[Decide whether the release breaks open work](upgrades-and-recovery.md#decide-whether-the-release-breaks-open-work).
A `promise_repair_repeated` log line can be the first sign of this.

A failed run reports its `kind` and real message. `modelFailure` covers a provider
failure after up to 21 minutes of retries, and `internal` is a refusal inside
Lightspeed that the `session_step_refused` error log also records. A call that answers
`operationPending` has not failed: it was held, usually behind a compaction, and
retrying it with the same submission ID is safe.

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

If session creation reports `modelDefaultUnset`, select an explicit session
or profile model, or configure **Models → Defaults → Agent runs**. A provider
key alone does not choose a model. Voice preparation and dictation need the
separate **Speech-to-text** default.

For context-length errors, inspect the effective compaction policy and recovery
attempts in session settings. Enabled sessions can compact and retry within
the same run; Disabled sessions do not recover automatically. A protected input larger
than the usable window can still fail, and the failure names the largest entry. A run
failing because the session holds too many context entries has reached the
2,000-entry limit: enable compaction or request one. See
[Manage long conversations](../using-lightspeed/sessions-and-runs.md#manage-long-conversations).

If a specific entry keeps making the provider reject requests, stop active
work and use [CLI context repair](../using-lightspeed/cli.md#repair-rejected-context)
to inspect and replace a user message or tool result. This preserves the
event history; do not rewrite the stored log to repair active context. A provider
refusing a request after a model switch fails the run with its message, and switching
back is enough.

If discovery succeeds but an agent has no expected tool, inspect its profile's
capability grants and the MCP server/tool selection. Registering a server
does not grant every session permission to use it. Also distinguish a tool
waiting for approval from one that was never exposed.

For private MCP endpoints, runtime discovery and execution require both the
server's private-network opt-in and the runtime's host/CIDR allowlist. An
"outbound hostname resolved to a forbidden address" failure means neither the
server's hostname nor an address it resolves to is covered by
`LIGHTSPEED_MCP_PRIVATE_NETWORKS`; every resolved address must fall inside a
listed range unless the hostname itself is listed. An OAuth sign-in uses the
same list and checks each endpoint by its own host, so the same failure during
sign-in usually means the authorization server is not listed even though the MCP
server is. Provider-hosted execution requires the model
provider to reach the MCP endpoint, which is a different network path from
the runtime reaching it. [Tools and MCP](../using-lightspeed/tools-and-mcp.md)
explains execution modes and discovery behavior.

## Execution environments

Begin with the environment's source, state, status, and state message. A
paused or stopped VM wakes when a tool connects to it, so a call that keeps
waiting points at the provider's data route; a disconnected registered
machine needs its daemon; a provisioning failure needs the provider and
template that created it.

For a registered daemon, inspect its own output and both public WebSocket
paths. The control connection can be established while data routing is still
broken. Verify WebSocket upgrades at the proxy and check the workers'
private gateway URL and token. If you run replicated gateways, also check
that peers can reach each other. Keep the daemon state directory so a
restart can prove the same identity.

For Incus, inspect provider `/health`, whether the provider is available to
the universe (to every universe, or to this one through a binding), which its
environments need to be reachable, the selected template,
instance boot, and guest daemon. Follow the specific procedures in
[Bring your own compute](../environments/bring-your-own-compute.md),
[Incus VMs](../environments/incus-vms.md), and
[Networking and ingress](../environments/networking-and-ingress.md).

If a process cannot see a workspace file, check which filesystem contains it.
VFS workspace files do not automatically appear on an execution environment's
disk. Transfer required inputs explicitly and verify the process working
directory. [Processes and jobs](../environments/processes-and-jobs.md) also
explains why a daemon restart interrupts persisted active jobs and loses
interactive process handles.

## Blob writes

If the runtime starts successfully but a larger file, attachment, model
payload, or diagnostic dump fails with
`blob exceeds inline threshold (65536 bytes) but no object store is configured`,
configure S3-compatible or Azure Blob storage. PostgreSQL-only
deployments accept blobs up to 64 KiB. Follow [Choose the blob backend](configuration.md#choose-the-blob-backend)
and retry a controlled write after restarting the affected processes.

When object storage is already configured, distinguish a failed write from
a missing existing object. Check its endpoint and permissions for writes;
for reads, also check the exact physical location retained by the blob catalog.
A bucket or endpoint change does not copy the old bytes.

## Retention and cleanup

Closing a session retains history unless a deletion policy later removes it.
Automatic deletion waits for the root's configured deadline and for its
retained descendant tree to be closed, and a warning names up to eight roots held back
by an open descendant. Blob collection then follows durable references and its own
grace period; repeated reads do not renew that period.

A machine's lifecycle is separate. Inspect environment ownership, retention,
and provider cleanup before expecting a session close to destroy a VM. A
bring-your-own machine remains under its operator's control. The
[retention guide](operations.md#manage-retention-and-blob-collection) and
[environment cleanup guide](../environments/power-and-cleanup.md) explain the
individual decisions.

If an archived universe still runs work, that is expected. Follow
[Managing universes](multi-tenancy.md#archive-and-delete-a-universe) to retire
it and check provider cleanup.
