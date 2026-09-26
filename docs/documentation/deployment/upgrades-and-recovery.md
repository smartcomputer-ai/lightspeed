# Upgrades and recovery

A Lightspeed upgrade has to preserve work already in progress. That includes
database records, Temporal histories, encrypted credentials, stored content,
and machines created by earlier runs. Keep a recovery copy of that state and
check whether the new release can read it before replacing the running code.

The procedure below continues the [single-host installation](self-hosting.md)
and uses a planned maintenance window. The current runtime does not configure
Temporal worker version routing, so use a coordinated rollout unless the target
release documents another supported procedure.

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

<a id="upgrade-from-v02-to-v03"></a>

## Check compatibility before reusing state

The current development code uses a consolidated runtime schema and the
Platform-owned access model. It has no automatic conversion from the former
core identity directory, resource grants, or execution identities. A migration
number or product version alone cannot identify a compatible database: the
runtime also compares applied migration names and checksums.

For disposable development state, stop the launcher and use `./dev.sh reset`
to recreate the local databases and clear the development object-store prefix.
Retained Temporal histories may still contain old payloads; create fresh work
with the new code. For an installation whose data must survive, preserve the
old executable and recovery set and validate a conversion against copies before
starting new workers. The maintenance procedure below assumes the target
release supports the retained state; it does not supply that conversion.

When installing a tagged release, use the upgrade notes shipped with that tag.
Instructions for an older release's migrations do not apply to this development
baseline. Likewise, an unchanged API or environment protocol identifier does
not guarantee compatibility with saved session configuration or workflow
histories. Use matching clients and review daemon changes even when protocol
mismatch would not trigger an automatic daemon update.

## Preserve a complete recovery set

Lightspeed has no integrated backup/restore command. Use the backup procedures
for your PostgreSQL, Temporal, object store, and compute infrastructure, and
record how their recovery points fit together.

| Material | Why recovery needs it |
| --- | --- |
| Runtime PostgreSQL database and migration ledger | Universe records, sessions/events, checkpoints, profiles, workspace references, blob catalog and inline content, credentials, keys, bots, channels, and environment state. |
| Platform PostgreSQL database and migration ledger | Login accounts and sessions, external login records, universe memberships and roles, and universe display/routing metadata. |
| Temporal persistence and namespace configuration | Workflow histories, timers, schedules, and in-flight orchestration. Runtime PostgreSQL records are not a documented replacement for lost Temporal state. |
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

1. Restrict incoming user and webhook traffic. Stop connector ingress and
   other clients that submit work. Remember that runtime timers and schedules
   can produce work independently of the public HTTP listener.
2. Inspect active runs, environment jobs, bot activity, and channel deliveries.
   Let important work finish, or cancel it and inspect effects
   already performed. Closing a session is a permanent lifecycle operation,
   so do not use it as a temporary maintenance pause.
3. Stop all application writers and workers, including separately deployed
   connectors and Configurator services. Keep the durable infrastructure
   available for the migration and backup procedure.
4. Capture or confirm the coordinated recovery set described above.
5. Apply runtime migrations with the target image, then recreate the
   application containers from that same release.
6. Verify existing data and useful work before reopening ingress. Inspect
   delayed bot and channel work as processing resumes.

For the two application containers in the self-hosting recipe:

```bash
docker stop lightspeed-platform lightspeed-runtime
```

The runtime container's configured stop signal is SIGINT. Its worker shutdown
has a bounded wait, so a completed `docker stop` does not prove every external
effect finished. Machine processes can also outlive a runtime stop; inspect
and manage them as part of the maintenance plan.

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

<a id="upgrade-environment-daemons-deliberately"></a>

## Upgrade environment daemons

An outbound daemon and its gateway must agree on the environment protocol.
The release metadata records that protocol, and the gateway reports a mismatch
when an incompatible daemon connects.

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
directory, state directory, and identity retained. An attempt limit prevents
a stale discovery document from causing an upgrade loop. See the
[daemon reference](../reference/environment-variables.md#environment-daemon)
for discovery URL and private-CA settings.

Passive Incus guests do not use outbound automatic upgrade. Update the image
template for new VMs and manage existing guest binaries through their machine
administration path. The supplied service's permissions also restrict where
it can write, so an in-place executable replacement may require administrator
action.

Daemon identity persistence and process persistence are different. On daemon
restart, persisted nonterminal jobs become `Interrupted`; interactive process
handles were held in memory. Retaining the state directory preserves identity
and recorded output, not a running operating-system execution. See
[Processes and jobs](../environments/processes-and-jobs.md) before scheduling a
machine update around active work.

## Recover a failed upgrade

Keep ingress and workers stopped while deciding whether to repair the target
release or restore the recorded recovery set. Preserve the exact error and
the old and new manifests. A configuration error may be corrected without
restoring data; a schema or workflow compatibility failure needs a different
decision.

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
work. Similarly, the promise reaper does not automatically restart an arbitrary
failed session workflow. Investigate its Temporal history and stored state
before choosing to close it and create replacement work.

| Failure | Next step |
| --- | --- |
| Runtime says migration is required | Run the intended release's explicit migrator, then its schema diagnostic. |
| A later migration fails | Preserve logs and inspect with the same target release; earlier migrations may already have committed. |
| An older image rejects the schema | Use a compatible image or restore the complete earlier recovery set. |
| Existing credentials no longer decrypt | Restore the matching master key and original universe mappings. |
| Sessions load but attachments fail | Verify object-store data and physical locations as well as the database catalog. |
| Daemon upgrade cannot find a download | Verify deployment discovery, target archive URL, checksum metadata, and executable permissions. |
| Restored listeners are healthy but work stalls | Verify namespace, workflows, role pollers, retained blobs, and optional service state through a complete test task. |
