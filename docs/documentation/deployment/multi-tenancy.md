# Managing universes

A universe has a runtime UUID and belongs to one Platform account. Each
account can contain many universes. The runtime owns sessions and resources;
the Platform owns account membership and universe grants. Preserve that association when adopting,
recovering, or retiring a universe.

Each universe also has a handle: a readable name, unique within the
deployment, that appears in its Platform URL and that authenticated API clients
may use in place of the UUID. A handle is 1 to 63 lowercase
letters, digits and hyphens, and can be changed; the UUID cannot. Because the
UUID usually differs between deployments, the handle is what deployment
automation should use to address the same universe in development, test and
production. See
[Runtime tokens and service access](../access-and-security/api-keys-and-service-access.md)
for how a request selects a universe.

For the isolation model and shared infrastructure, see
[Tenant isolation and data protection](../access-and-security/tenant-isolation-and-data-protection.md).
For sign-in and membership, see [People and roles](../access-and-security/people-and-roles.md).

## Create a universe

Creators and Account Admins create universes using **New universe**, selecting
an account and an initial Owner. Platform Admins can provision for any account
with an explicit initial Owner. Creation observes the account quota and
protection policy. Owners grant existing account members access on **Members**.
Creating a runtime universe directly through the API or server CLI creates
only the runtime record; use **Adopt** to connect it to the Platform. Adoption
keeps the universe's handle. The handle is chosen at creation, or derived from
the UUID when none is given, and is renamed afterwards under **Settings →
General** or with `lightspeed universe set-handle`.

## Keep Platform and runtime records aligned

Creating a universe through the Platform creates the runtime universe and
records the association in the Platform database. Because these are separate
systems, an interrupted operation or a partial restore can leave one side
without the other.

**Platform admin → Universes** reconciles the configured runtime inventory.
**Adopt** connects an existing runtime universe to a selected account with an
explicit initial Owner, without replacing its contents. **Create in engine** creates an empty missing runtime
universe. It cannot restore deleted sessions or files, or recreate a universe whose
UUID has been permanently deleted. All universes use the deployment's configured
runtime endpoint.

Preserve runtime UUIDs and their Platform mappings during recovery. Secret
encryption and workflow identities depend on those UUIDs, so copying records
under a newly invented UUID is not a supported tenant migration. See
[Upgrades and recovery](upgrades-and-recovery.md) for the full recovery set.

## Archive and delete a universe

Archiving changes the universe's Platform status and hides it from the
ordinary switcher. Existing URLs and API requests remain callable, and the
runtime has no corresponding archived state. Bots, schedules, channels, and
previously admitted work can continue. Archiving is therefore an organization
step, not an access-revocation or shutdown mechanism.

Deletion does not require archiving, closing sessions, stopping bots, or closing
environments first. Retain any content you need before proceeding. A Platform
administrator selects **Delete** under **Platform admin → Universes** and types
the universe's handle to confirm. Through the CLI:

```bash
lightspeed universe delete my-universe --wait
```

Use `--yes` to omit the interactive handle confirmation. `--wait` polls until
cleanup finishes; an optional duration bounds how long the CLI waits.

Deletion immediately refuses new universe requests, prevents new daemon
registrations in that universe, and frees the handle for reuse. The universe then appears as **deleting** in the Platform
admin inventory, with its current cleanup step and any waiting detail. It is
hidden from ordinary members. Cleanup removes bot schedules, terminates the
universe's open workflows, closes environments, clears object storage, and
finally removes its database rows. Storage failures keep the universe in the
storage step for retry.

Environment cleanup is best effort. Connected registered daemons receive a
closure notice; disconnected daemons cannot reconnect with their old registration
key. Providers are asked to close provisioned machines. After bounded attempts
or two minutes, deletion proceeds even if a provider has not confirmed cleanup.
The permanent deletion record identifies any unconfirmed machines by provider
and target ID for an operator to remove.

API clients call `deployment/universes/delete` once, then poll
`deployment/universes/read` by UUID. A deleting universe remains readable through
that deployment method. Once deletion finishes, reading the UUID returns `gone`
with its permanent record; a UUID that never existed returns `notFound`.
Repeating deletion is safe, and a deleted UUID cannot be created again. Use
`lightspeed universe list --deleted` or `deployment/universes/deleted/list` to
inspect deletion records, including resource counts, attribution and cleanup
results.

Protected universes refuse deletion, even for Platform Admins. Owners can
enable protection; only an Account Admin or Platform Admin can clear it.
An unprotected universe can have an expiry timestamp for automatic deletion.
Protection and expiry are mutually exclusive. Clear protection and delete in
separate requests. Configuration documents protect new universes by default.

The Platform periodically follows the runtime's inventory, including deletions
requested directly through the API or CLI. It removes that universe's grants and setups only after the runtime confirms `gone`. The containing
account, its members, and its other universes remain. A universe
missing from an empty or different runtime remains marked missing so that an
accidental configuration change does not discard memberships.

Deletion does not remove:

- Temporal histories, which remain until the namespace retention period ends;
- machines hosting registered daemons or external endpoints;
- provider resources shared per universe, such as an Incus project, network,
  ACL and profile;
- connector-local authentication directories, including WhatsApp state;
- third-party state, including OAuth-issued tokens, model-provider usage, and
  effects agents already performed outside Lightspeed;
- database and object-store backups.


<a id="follow-a-request-into-its-universe"></a>
<a id="how-the-boundary-is-represented"></a>
<a id="what-the-deployment-shares"></a>
<a id="credentials-and-billing"></a>
<a id="machines-and-networks"></a>
<a id="access-inside-a-universe"></a>
<a id="evaluate-the-boundary-for-your-deployment"></a>

## Access and isolation

The explanation of universe boundaries now lives in
[Tenant isolation and data protection](../access-and-security/tenant-isolation-and-data-protection.md).
[Private and shared work](../access-and-security/private-and-shared-work.md)
explains who can read and control sessions inside a universe.
