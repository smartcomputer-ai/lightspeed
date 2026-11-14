# Upgrades and recovery

A Lightspeed upgrade touches state that outlives the running code: database
records, Temporal histories, encrypted credentials, stored content, and machines
created by earlier runs. Keep a recovery copy of that state, and settle before
you deploy whether the new release can keep the open work in its Temporal
histories. Lightspeed has no tool that replays histories against a new build, so
that compatibility exists only when a release says it does.

The procedure below continues the [single-host installation](self-hosting.md)
and uses a planned maintenance window. The runtime does not configure Temporal
worker version routing, so stop every old worker before the first new one starts.
On Kubernetes that means the `Recreate` deployment strategy: with the default
rolling update, both builds poll one task queue during the overlap.

## Retain a coherent release

Build or obtain the target release before the maintenance window. Keep its
manifest together with the deployed release's manifest and images. The
manifest records the source revision, API protocol and contract revision, runtime and
Platform schema revisions, Platform migration baseline, environment protocol,
image references, and binary checksums.

Select runtime and Platform from the same release. Review compatible
Configurator, connector host, CLI/client, environment provider, and daemon
artifacts when those components are installed. A mutable image tag alone is
insufficient to reproduce the installation; retain revision-specific tags or
the manifest's immutable image references.

The [build and release guide](../../releasing.md) describes artifact
construction. The target release's metadata and migration instructions decide
compatibility with the state you actually have. Platform migration checks cover
a fresh installation and an upgrade from the baseline named in
`LIGHTSPEED_PLATFORM_UPGRADE_FROM`. They do not test every historical schema.
A database from another baseline needs its own validated migration.

## Decide whether the release breaks open work

Temporal rebuilds a workflow by replaying its history through the current code.
If a release changes the commands that code issues, or the types recorded in
history, an open execution cannot replay past that point. So each release is one
of two kinds, and nothing at runtime tells them apart. Compare the deployed
release with the target before the maintenance window, from a source checkout:

```bash
scripts/release/workflow-compat.sh <deployed-tag> [<target-ref>]
```

The script prints the changes it found, then one word. It compares
`temporal-workflow`, the workspace crates that crate depends on (`harness`, `api`,
`bots`, `channels`), and the `temporalio` packages in `Cargo.lock`.

| Result | Meaning | What to do |
| --- | --- | --- |
| `restart-in-place` | None of that changed, so open executions replay on the new code. | Keep the current namespace. Open work carries on after the restart. |
| `breaking` | Something changed, and retained histories may no longer replay. | Drain first, then start on a new Temporal namespace. |

This is a diff and not a replay check, and it errs both ways. Any change to those
crates prints `breaking`, including one to a test or a generated file, and a false
alarm only costs you the namespace change. It sees no other third-party version,
so read the `Cargo.lock` diff before trusting `restart-in-place`: a dependency
bump that changes workflow behaviour would still print it.

Release notes state which kind a release is. If the call turns out wrong, `/health`
stays `ok` and affected executions fail one at a time as they are touched: their
workflow tasks keep failing, commands to them fail or time out, and the Temporal UI
shows a nondeterminism error, or a panic saying an activity payload does not convert
with this build. Stored reads keep working. Moving to a new namespace afterwards heals
this the same way as if the release had been called breaking.

### Move a breaking release to a new namespace

Create the namespace in your Temporal service. Set `TEMPORAL_NAMESPACE` to it on every
runtime process, on the connector host, and on any custom worker running your workflow
tools, in the same step as the release change. No history replays on changed code, so
no ordering between old and new builds matters, and the old namespace stays available
for inspection until you retire it. Bot schedules left behind in it start workflows
that nothing runs, so retire it once you are done with it.

Keep the runtime database unless the release notes require otherwise. The session
log lives there, so transcripts, session reads and force close keep working, and
Temporal schedules come back when the runtime starts. Workflows do not come back on
their own:

| Open work | What happens on the new namespace |
| --- | --- |
| Session | Reads report `executionFailure: missing`, and runs are refused with `sessionExecutionStopped`, until someone [restarts it](#restart-a-stopped-session). |
| Run that was executing | Fails at restart with a message saying its workflow stopped, so no tool call runs twice. |
| Run parked on an approval, promise or workflow | Keeps waiting after restart. A reply its receiver sent in the meantime is lost, so that wait lasts until the promise's deadline or a cancel. |
| Sub-agent, environment job and other started workflows | Do not continue. The parent's promise ends at its deadline or on a cancel. The job's process on its machine is not stopped by this. |
| Transcription job | Lost, along with its idempotency key, so a retry starts a new job. |
| Bot controller | Starts again at the bot's next event, moving past every retained main and per-key generation, so its conversations carry on in new sessions with no earlier context. |
| Undelivered bot events | Not rescanned. They stay stored without being delivered, and need replaying from Activity. |
| Bot dedupe tails, daily budget counts | Reset. |
| Group `/activation` changes in a chat | Revert. |

Old main sessions stay open until someone closes them. Drain first, as the procedure
below describes: everything in this table is much smaller on an idle installation.

<a id="upgrade-from-v02-to-v03"></a>

## Check compatibility before reusing state

The current development code uses a consolidated runtime schema and the
Platform-owned access model. It has no automatic conversion from the former
core identity directory, resource grants, or execution identities. A migration
number or product version alone cannot identify a compatible database: the
runtime also compares applied migration names and checksums.

The current development code also renames stored event kinds, status values,
wire values and bot columns, and bounds what reduced state keeps. It has no
conversion for any of that: a log written by an earlier build does not decode,
and earlier bot rows fail the new constraints at `migrate`. Moving to it takes a
fresh runtime database and a new Temporal namespace. Every API enum and error kind
is now camelCase, so clients and dashboards reading the old values need updating.

A request an API key makes for itself is now attributed to the key's id and
scope kind, no longer to the display prefix of its secret. `migrate` rewrites
the stored attributions in the runtime database, including those inside session
events, by looking up the key that holds each recorded prefix. A record whose
key has had its secret replaced since cannot be matched and becomes an internal
attribution that quotes the old prefix. Temporal histories are not rewritten: a
session, or a transcription job, in which a key acted without asserting an
actor does not replay, so treat the release as
[breaking](#move-a-breaking-release-to-a-new-namespace). Clients that read
`createdBy`, `requestedBy` or `decidedBy` find `keyId` and `scopeKind` where
`prefix` used to be.

Creating or declaring an API key now requires its method groups. A
`deployment/api-keys/create` or `deployment/api-keys/put` request, an
`api-key create`, `put` or `provision` command, or an `apiKeys` entry of a
configuration document that leaves them out is refused where it used to
receive every group of the key's scope. Add `"groups": "all"`, `--all-groups`
or `groups: all` to scripts and documents that relied on the omission, or
list the groups. Stored keys keep the groups they hold. `api-key provision`
also no longer prints a secret that was supplied through
`LIGHTSPEED_BOOTSTRAP_API_KEY`, so a start-up job that read the secret back
from the command's output must use the value it supplied. See
[API keys and service access](../access-and-security/api-keys-and-service-access.md).

In `authenticated` mode a request without a valid key is now answered with
HTTP 401 before its body is read, where it used to receive HTTP 200 with a
JSON-RPC error. The response still carries the `unauthenticated` error, with a
null `id`. A key that cannot be checked, for example because the database is
unreachable, is answered with HTTP 500 the same way. The CLI and the SDKs of
this release report the 401 as the typed `unauthenticated` error; an SDK built
before it raises a transport error instead, and a client that calls `/rpc`
itself must read the status. See
[Gateway modes](../access-and-security/api-keys-and-service-access.md#gateway-modes).

An extra header of a model provider's endpoint is now a tagged value, a
`literal` or a `credential`, the form an MCP server's request headers already
have, where it used to be a plain string. API clients and configuration
documents that send a header as a bare string are refused and must send the
tagged form. `migrate` does not convert stored headers: remove the extra
headers of a model provider before upgrading and add them again afterwards.

On both a model endpoint and an MCP server, a header named `api-key` or
`x-api-key` now accepts only a `credential` (as does `proxy-authorization` on a
model endpoint; an MCP server reserves that name). These names carry a secret,
and a `literal` is returned to every reader of the provider or server.
`migrate` stops with a constraint error if an MCP server still stores a plain
value under one of them: before upgrading, save the value as a token credential
and make the header name that credential. A
`lightspeed model provider put --header` or `lightspeed mcp server put --header`
script keeps working unless it passes one of these names, which take
`--header-credential`. See
[Extra headers](../using-lightspeed/models-and-credentials.md#extra-headers)
and
[custom request headers](../using-lightspeed/tools-and-mcp.md#authenticate-with-custom-request-headers).

Only two things can now be bound into an environment: a credential created as
retrievable, and the API key of a model API key provider. The runtime checks
this when a binding is made and each time it is resolved. `migrate` records
every credential that is already bound into an environment as retrievable, so
existing bindings keep working; from then on such a credential can also be
leased by trusted services. `migrate` deletes bindings that named a stored
secret directly: that source, and the CLI's `--secret-id` flag, no longer
exist, so list such bindings before upgrading and bind a retrievable
credential under the same variable afterwards. A binding to any other
provider's credential, such as a GitHub App's private key, is kept but refused
when it is resolved, and every command and job in that environment fails with
a message naming the variable until the binding is removed; bind a GitHub
installation credential in its place. A brokered credential that was not bound
stays brokered and cannot be bound later, which includes GitHub installation
credentials granted before the upgrade: add the credential again as
retrievable. Scripts and configuration documents that create a credential for
an environment must say so, with `--retrievable` or `exposure: retrievable`.
A configuration document written before the upgrade that binds a credential
into an environment declares that credential without `exposure`, which reads
as brokered, while `migrate` has stored it as retrievable. `config diff` and
`config apply` stop at that difference and name the stored value: add
`exposure: retrievable` to the credential's existing entry. The credential
keeps its id and its token. `config export` leaves out a binding to another provider's credential, so the
exported document can be applied; applying it to an environment that lists
other bindings removes the left-out one.
See
[Choose a credential source](../environments/credentials.md#choose-a-credential-source).

`LIGHTSPEED_MCP_OAUTH_ALLOW_PRIVATE_NETWORKS` is removed and no longer read; a
value left in the environment has no effect. MCP OAuth requests (metadata,
client registration, token exchange and refresh) now follow
`LIGHTSPEED_MCP_PRIVATE_NETWORKS`, decided for each URL by its own host. A
deployment that set the old switch must list its internal MCP servers and their
authorization servers, by hostname or address range, or sign-in and token
refresh toward them fail with "outbound hostname resolved to a forbidden
address". See the
[environment-variable reference](../reference/environment-variables.md).

A skill's `name`, `description` and `short_description` now have fixed length
limits and may not contain control characters. A skill outside them leaves the
catalog at the next refresh with a warning that names its file. See
[Add a skill](../using-lightspeed/workspaces-and-skills.md#add-a-skill).

The **Bots** and **Channels** features of a universe now start switched off,
and the Platform refuses bot and channel requests from members while a switch
is off, where it used to only hide the pages. The new default applies to
existing universes too: after the upgrade, a universe that never stored its
own choice has both features off, and its bot and channel pages stay hidden
until an Admin turns them on under **Settings → General → Features**. Do that
in every universe that uses bots or chat channels. Bots that are already
running keep running in the runtime during that time, because the switch
governs only what members do through the Platform. A deployment that uses
neither feature can also stop running the `bots` and `channels` roles; see
[Runtime roles and scaling](overview.md#runtime-roles-and-scaling).

A trigger's ingest path and pairing code are no longer part of the trigger
that `bots/triggers/read`, `bots/triggers/list` and `bots/triggers/delete`
return. A client that read `ingestPath` or `pairingCode` there must call
`bots/triggers/secrets/read`, a bot-management method, or take the values from
the answer of `bots/triggers/put` (`secrets`) or `bots/create`
(`triggerSecrets`). A bot that may change its own triggers no longer sees
either value when it lists them; it is told a URL or code once, by the call
that created it, and only in its Main conversation. See
[Receive a webhook](../using-lightspeed/bots-and-triggers.md#receive-a-webhook).

`lightspeed-envd` no longer loads a `.env` file from its working directory. A
daemon that took `LIGHTSPEED_ENVD_*` settings from one starts without them:
pass them through the process environment or the command line, for example
with a systemd `EnvironmentFile`. The daemon's file tools now also refuse the
registration key file it was started with and, on Linux, the `/proc` and
`/sys` filesystems, under any filesystem root; a command's working directory
follows the same rule, and what a started command reads does not. See
[Choose the access level](../environments/using-environments.md#choose-the-access-level).

`TEMPORAL_ADDRESS` now names a host and port only. An address with user
information, a path or a query is a startup error in the runtime and in the
connector host. A bare host written without a port now connects to port
`7233`, where the runtime used to fall back to port `80`. The connection can
also be secured with TLS, a client certificate and an API key; with none of
those settings it is unencrypted as before. See
[Secure the Temporal connection](configuration.md#secure-the-temporal-connection).

The web app no longer displays images that Markdown loads from another site.
In transcripts, reasoning, and file and blob previews such an image appears as
a link that names its host; images recorded with the session or stored in its
files are displayed as before. Enabling **Virtual File System** or
**Environments** in the profile and session editor no longer turns on **Prompt
loading** and **Skill discovery**; saved configurations keep their settings.

For disposable development state, stop the launcher and use `./dev.sh reset`
to recreate the local databases and clear the development object-store prefix.
Retained Temporal histories still hold the old payloads, so also point
`TEMPORAL_NAMESPACE` at a new namespace. For an installation whose data must
survive, preserve the old executable and recovery set and validate a conversion
against copies before starting new workers. The maintenance procedure below
assumes the target release supports the retained state; it does not supply that
conversion.

When installing a tagged release, use the upgrade notes shipped with that tag.
Instructions for an older release's migrations do not apply to this development
baseline. Likewise, an unchanged API or environment protocol identifier does
not guarantee compatibility with saved session configuration or workflow
histories. Use matching clients and review daemon changes even when protocol
mismatch would not trigger an automatic daemon update.

## Review model and context configuration

Remove `LIGHTSPEED_CHAT_PROVIDER` and `LIGHTSPEED_CHAT_MODEL` from the runtime
environment before starting current workers; both variables are retired and
cause startup to fail. After the runtime is available, save the intended
provider/API/model route in each universe's **Agent runs** default using
**Models → Defaults** or `lightspeed model defaults set agent-run`. Configure
**Speech-to-text** separately for dictation and channel voice messages.
Deployment provider credentials remain supported, but do not choose a model.
See [Configure universe model defaults](../using-lightspeed/cli.md#configure-universe-model-defaults).

Existing sessions retain their stored model. New sessions and configuration
replacements resolve an omitted compaction policy to automatic standalone
compaction; preserve an explicit Disabled setting when automatic compaction and
context-limit recovery should stay off. Review these settings when applying a
profile or replacing configuration during an upgrade.
See [Manage long conversations](../using-lightspeed/sessions-and-runs.md#manage-long-conversations).

Audio integrations must transcribe before session admission. Run start,
context append, and steering reject raw audio; use the
[standalone transcription flow](../integrating-and-extending/api-and-typescript.md#transcribe-audio-before-submitting-session-input)
and submit prepared text.

## Enable company sign-in on an existing Platform

The company-identity migration adds identity provenance, session revocation
versions and durable access records. Existing users remain local and keep
their memberships; the migration does not grant company or emergency access.
The required Platform revision is `LIGHTSPEED_PLATFORM_SCHEMA_REVISION` in
the target release's [metadata](../../../release/metadata.env). Startup applies
the generated Platform migrations before serving requests.

Before enabling OIDC, designate an existing local password admin through
`LIGHTSPEED_PLATFORM_ADMIN_EMAIL` and verify its emergency login. Use a
separate email from the company account: existing local and company accounts
are never linked by email. Resolve any email collision explicitly before
company sign-in; no automatic identity or membership transfer is provided.
Ordinary local accounts and their old sessions cannot access an SSO-enabled
Platform. Keep their records until you have reviewed the required migration.

Configure and test the provider as described in
[Company sign-in (SSO)](../access-and-security/single-sign-on.md).
Verify successful and denied login, both entitlements, emergency access,
suspension and expiry before opening access. Disabling OIDC does not turn
company accounts into password accounts.

## Preserve a complete recovery set

Lightspeed has no integrated backup/restore command. Use the backup procedures
for your PostgreSQL, Temporal, object store, and compute infrastructure, and
record how their recovery points fit together.

| Material | Why recovery needs it |
| --- | --- |
| Runtime PostgreSQL database and migration ledger | Universe records, sessions/events, checkpoints, profiles, workspace references, blob catalog and inline content, credentials, keys, bots, channels, and environment state. |
| Platform PostgreSQL database and migration ledger | Login accounts and sessions, external login records, universe memberships and roles, universe display/routing metadata, and durable access history. |
| Temporal persistence and namespace configuration | Workflow histories, timers, schedules, and in-flight orchestration. Without it, open sessions can [restart from the runtime database](#restart-a-stopped-session), with [these losses](#move-a-breaking-release-to-a-new-namespace). Nothing rebuilds the rest. |
| Configured object-store content | The bytes referenced by object-backed blobs. Restoring their database catalog does not reconstruct missing objects. |
| Runtime master key | Existing encrypted secret values and grant tokens need the same `LIGHTSPEED_SECRETS_MASTER_KEY`. |
| Deployment configuration and other stable secrets | Platform authentication secret, environment routing token, service addresses, task queues, provider configuration, and credentials. |
| WhatsApp connector state, when used | The complete authentication directory and unchanged media-locator key preserve linked-device state and the ability to resolve sealed media locators. |
| Machine/VM storage and daemon state | Real environment files, daemon identity, and persisted job records/output live outside the VFS and application databases. |
| Release artifacts | The exact compatible images, executables, and manifests needed to run the restored state. |

The Incus provider is stateless as a controller, but its configuration and TLS
client files, the Incus inventory, and VM disks still need an infrastructure
recovery plan. A runtime database backup does not contain those disks.

Preserve universe UUIDs and their Platform mappings. Secret encryption binds
ciphertext to the universe UUID, secret ID, and kind, so relabeling restored
records with a new UUID can make their credentials unreadable. Runtime API
keys are stored as hashes; preserving the database keeps their recognition
state, but does not recover a lost client's plaintext secret.

A useful recovery set has a defined relationship between its snapshots.
Independent database dumps taken while writers continue do not by themselves
establish consistency with each other, Temporal, or object storage. Quiesce
application writes and incoming work according to the infrastructure's backup
procedure, and retain the configuration and release record from that point.

## Rehearse against isolated infrastructure

Restore a copy before relying on the recovery procedure. Use separate
PostgreSQL databases, Temporal state, and object storage, and keep external
effects contained during the rehearsal. A restored bot or connector can still
hold valid credentials; starting it against production chat accounts, Incus,
or webhook destinations can perform real work.

Start with existing data: sign in, open a known universe and session, read a
workspace file and attachment, and verify that stored credentials can be
resolved. Then exercise a small controlled run and the optional capabilities
the installation needs. Check the old data as well as newly created records.

Record the steps, durations, and any manual reconciliation. A successful
fresh-install test does not establish that an older history or migration
ledger can be recovered.

## Upgrade the single-host installation

Use the retained `runtime.env` and `platform.env` from self-hosting, and set
`LIGHTSPEED_RELEASE_ID` to the already-built target image tag. Keep the old tag
and configuration in the deployment record.

1. [Decide whether the release is breaking](#decide-whether-the-release-breaks-open-work).
2. Restrict incoming user and webhook traffic. Stop connector ingress and
   other clients that submit work. Remember that runtime timers and schedules
   can produce work independently of the public HTTP listener.
3. Drain. Inspect active runs, environment jobs, bot activity, and channel
   deliveries. Let important work finish, or cancel it and inspect effects
   already performed. Answer pending approvals you care about. This matters most
   for a breaking release, which loses what is still in flight. Closing a session
   is a permanent lifecycle operation, so do not use it as a temporary maintenance
   pause.
4. Stop all application writers and workers, including separately deployed
   connectors and Configurator services. Keep the durable infrastructure
   available for the migration and backup procedure.
5. Capture or confirm the coordinated recovery set described above.
6. Apply runtime migrations with the target image. For a breaking release, create
   the new namespace and set `TEMPORAL_NAMESPACE` on the runtime, the connector host
   and any custom worker.
   Then recreate the application containers from that same release.
7. Verify existing data and useful work before reopening ingress. Inspect
   delayed bot and channel work as processing resumes. After a breaking release,
   restart the open sessions you still want.

For the two application containers in the self-hosting recipe:

```bash
docker stop lightspeed-platform lightspeed-runtime
```

The runtime shuts down gracefully on SIGTERM, which Docker and Kubernetes both send
by default, and on SIGINT. Give it time to finish: the self-hosting recipe sets
`--stop-timeout 30`, and Kubernetes' default termination grace period is the same.
Its worker shutdown has a bounded wait, so a completed `docker stop` does not prove
every external effect finished. A foreground command whose tool call is dropped by
the shutdown is sent a terminate if the runtime lives long enough to send it, and
one that had already returned a handle keeps running. Inspect and manage machine
processes as part of the maintenance plan.

Once the recovery set is confirmed, run the target runtime's migration and
diagnostic commands from the deployment directory:

```bash
docker run --rm --network lightspeed --env-file runtime.env \
  "lightspeed-runtime:$LIGHTSPEED_RELEASE_ID" migrate

docker run --rm --network lightspeed --env-file runtime.env \
  "lightspeed-runtime:$LIGHTSPEED_RELEASE_ID" schema-version
```

`migrate` verifies the ledger, takes a database migration lock, and applies
pending migrations. Each migration commits separately. If one succeeds and
the next fails, the earlier change remains applied. Diagnose the failure and
rerun the same target release after correcting its cause; do not assume the
entire sequence rolled back.

`schema-version` inspects the result. It does not migrate and can exit
unsuccessfully when migration is still required. Normal runtime startup also
verifies the schema without applying migrations.

After successful migration, remove the stopped application containers so the
same service names can be reused:

```bash
docker rm lightspeed-platform lightspeed-runtime
```

Recreate the runtime using [Migrate and start the runtime](self-hosting.md#migrate-and-start-the-runtime),
starting at its `docker run -d` command. Recreate Platform using
[Start the Platform](self-hosting.md#start-the-platform). Retain the network,
protected environment files, persistent infrastructure, and target image tag;
the earlier network-creation and key-generation steps are not upgrade steps.
Platform applies its own database migrations when the new container starts.

Recreate installed optional services from their compatible artifacts. Check
health and logs, sign in, open an existing session, read existing content, and
complete a small run. Verify resumed workflow processing, machine access, and
connector account readiness where used. Reopen ingress after those checks and
observe the deferred work that follows.

## Restart a stopped session

A session whose workflow execution has stopped keeps its log and can come back. This
is how open sessions return after a breaking release, and also after an operator
terminates a workflow, a store rejection ends one, or Temporal data is lost. Nothing
does it for you: a stopped session stays stopped until someone asks.

`session/read` reports the stop in `executionFailure` (`failed`, `terminated`,
`timedOut` or `missing`), and a run start answers `sessionExecutionStopped`. Choose
**Restart** on the session's banner, or run:

```bash
lightspeed session restart "<session-id>"
```

Direct clients can call `session/restart`, which needs the Operator role. Retrying
`session/start` with the session's ID does the same for an ordinary session, for a
Contributor who can see it. Integrated sessions, including bot sessions, and sub-agent
sessions take `session/restart` only. A retried `session/start` for an integrated session
answers `sessionExecutionStopped` rather than restarting. Restart is safe to repeat: a session already running answers
`restarted: false`.

A new execution loads the session from the log. Then:

- A run that was not parked fails with a message saying its workflow stopped. A
  generation or tool call cut off by the stop may have finished and been paid for,
  and this is the accepted price of not repeating a paid call or a `git push`.
- A pending code tool call that is not safe to retry fails.
- A parked run keeps waiting, and queued runs start afterwards.
- Terminal notifications for the last 8 finished runs go out again, as do pushed
  invocations with a promise still pending. Receivers deduplicate them.

A restart does not recover what only the stopped execution held. A reply sent to a
parked run during the outage is lost. Pending promise cancellations are lost, so a
receiver may not hear of a cancel and a started execution may run until it ends by
itself. A session with no
run history cannot restart, because it may never have finished setup: force close it
and create a new one. A force close is permanent, so do not use it on a session you
want back.

<a id="upgrade-environment-daemons-deliberately"></a>

## Upgrade environment daemons

An outbound daemon and its gateway must agree on the environment protocol.
The release metadata records that protocol, and the gateway reports a mismatch
when an incompatible daemon connects. Provider controllers have their own
protocol number: redeploy every provider together with a runtime release that
changes it. A provider that runs idle policy also reads its daemons' idle
reports, so it moves with the daemon protocol as well. A provisioned machine
whose baked-in daemon speaks another version fails when it becomes ready,
with an error naming both versions, so recreate such machines from an
updated image.

`lightspeed-envd upgrade` installs the build named by the deployment's discovery
document. It verifies the archive checksum and checks the candidate's version,
source revision, target, and protocol before replacing the executable. A
separately running daemon service still needs a restart to use a manual update.

The discovery document is served by the deployment. Neither the runtime nor
Platform automatically serves `/.well-known/lightspeed-envd`, and the
self-hosting proxy recipe does not install it. Releases produce `envd.json`;
artifacts with null download URLs need usable HTTPS archive URLs supplied by
the serving deployment. Configure that route and validate its downloads before
relying on daemon upgrades.

`LIGHTSPEED_ENVD_AUTO_UPGRADE=true` opts an outbound daemon into upgrading on a
protocol mismatch. It does not update on every version or source change. The
daemon replaces its executable and re-executes with its arguments, working
directory, state directory, and identity retained. The daemon attempts one
upgrade per protocol version the gateway names, so a stale discovery document
cannot cause an upgrade loop. Network failures while fetching the document or
archive are retried; a checksum or build-fact mismatch stops the daemon. Keep
automatic upgrade off for daemons in containers and on read-only root
filesystems, and roll out a new image instead; see
[Run registered daemons](../environments/registered-daemons.md#provide-the-daemon-binary).
See the
[daemon reference](../reference/environment-variables.md#environment-daemon)
for discovery URL and private-CA settings.

Passive Incus guests do not use outbound automatic upgrade. Update the image
template for new VMs and manage existing guest binaries through their machine
administration path. The supplied service's permissions also restrict where
it can write, so an in-place executable replacement may require administrator
action.

Daemon identity persistence and process persistence are different. On daemon
restart, persisted nonterminal jobs become `Interrupted` and their surviving
processes are stopped. Earlier process handles report that the daemon
restarted; process groups they left running are reported as leftovers and
are not killed. Retaining the state directory preserves identity and recorded
output, not a running operating-system execution. See
[Processes and jobs](../environments/processes-and-jobs.md) before scheduling a
machine update around active work.

## Recover a failed upgrade

Keep ingress and workers stopped while deciding whether to repair the target
release or restore the recorded recovery set. Preserve the exact error and
the old and new manifests. A configuration error may be corrected without
restoring data. A schema failure needs a repaired or restored database. A workflow
that cannot replay needs [a new namespace](#move-a-breaking-release-to-a-new-namespace),
which leaves the database as it is.

The runtime migration ledger rejects schemas newer than its executable, gaps,
and changed migration names or checksums. There is no general downgrade
command. Returning to an old image after migration is therefore not a complete
rollback procedure.

Do not rewrite migration ledger rows to make a check pass.
`LIGHTSPEED_ALLOW_UNLEDGERED_SCHEMA` exists for intentionally externally
managed schemas; it does not migrate tables or bypass a corrupted or stale
ledger. Preserve a valuable historical database and establish a validated
migration path. Resetting a disposable development database is a separate
choice, not a production recovery method.

If restoration is required, restore the compatible databases, Temporal state,
objects, configuration, keys, and relevant machine/connector state together,
using the release recorded with that recovery point. Rehearse the restoration
in isolation first. Changing one store while leaving the others at a different
point can leave missing blobs, mismatched workflow state, or broken universe
mappings.

A restored database does not undo messages already sent, remote API changes,
or machine commands already executed. Inspect those effects before resubmitting
work. The promise reaper reports a failed session workflow and does not restart
it. Investigate its Temporal history and stored state, then
[restart the session](#restart-a-stopped-session) or close it and create
replacement work.

| Failure | Next step |
| --- | --- |
| Runtime says migration is required | Run the intended release's explicit migrator, then its schema diagnostic. |
| A later migration fails | Preserve logs and inspect with the same target release; earlier migrations may already have committed. |
| An older image rejects the schema | Use a compatible image or restore the complete earlier recovery set. |
| Existing credentials no longer decrypt | Restore the matching master key and original universe mappings. |
| Sessions load but attachments fail | Verify object-store data and physical locations as well as the database catalog. |
| Daemon upgrade cannot find a download | Verify deployment discovery, target archive URL, checksum metadata, and executable permissions. |
| Restored listeners are healthy but work stalls | Verify namespace, workflows, role pollers, retained blobs, and optional service state through a complete test task. |
| Workflow tasks keep failing after an upgrade, with a nondeterminism error or an activity payload that does not convert | The release was breaking. Move to [a new namespace](#move-a-breaking-release-to-a-new-namespace). A payload error also heals under a build that reads the payload. |
| A session reads fine but reports `executionFailure` | Its workflow has stopped. [Restart it](#restart-a-stopped-session). |
