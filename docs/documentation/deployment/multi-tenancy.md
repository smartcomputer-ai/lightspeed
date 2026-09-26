# Managing universes

A universe has a runtime UUID and a Platform organization linked to it. The
runtime owns the team's sessions and resources; the Platform owns its members,
roles, and display information. Preserve that association when adopting,
recovering, or retiring a universe.

For the isolation model and shared infrastructure, see
[Tenant isolation and data protection](../access-and-security/tenant-isolation-and-data-protection.md).
For sign-in and membership, see [People and roles](../access-and-security/people-and-roles.md).

## Create a universe

A Platform administrator creates universes under **Platform admin → Universes**.
Creation adds a runtime universe and a Platform organization, with the creator as an
Admin member. Universe admins then add existing accounts under **Members**.
Creating a runtime universe directly through the API or server CLI creates
only the runtime record; use **Adopt** to connect it to the Platform.

## Keep Platform and runtime records aligned

Creating a universe through the Platform creates the runtime universe and
records the association in the Platform database. Because these are separate
systems, an interrupted operation or a partial restore can leave one side
without the other.

**Platform admin → Universes** reconciles the configured default runtime inventory.
**Adopt** connects an existing runtime universe to the Platform without
replacing its contents. **Create in engine** creates an empty missing runtime
universe. It cannot restore deleted sessions or files. Universes using custom
gateway URLs can appear as `unchecked` in this inventory.

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

Retire the tenant's running work before permanently deleting its records:

1. Stop new submissions, disable or remove triggers, and stop external clients
   and connector ingress for the universe. Revoke runtime keys and remove
   unwanted Platform access.
2. Inspect active sessions, bot activity, channel deliveries, and environment
   jobs. Let required work finish or cancel it, accounting for
   effects it already performed. Close retired bots to disable their triggers,
   remove their schedules, and request controller teardown.
3. Close the universe's environments and confirm the intended machine cleanup.
   Provider-owned VMs require provider destruction; bring-your-own machines
   remain the operator's responsibility. Retain any files that must survive.
4. Archive the universe in the Platform. A platform administrator can then
   permanently delete it after the required data has been retained elsewhere.
5. Verify remaining infrastructure artifacts, including Temporal schedules
   and histories, provider inventory, connector-local state, and object storage.

The runtime purge terminates the session workflows it enumerates, deletes
catalogued external blobs, and removes runtime rows through database cascades.
It also attempts a best-effort cleanup of the universe's CAS prefix and evicts
the handling process's cached universe service. The Platform then removes its
organization and associated records.

That purge does not enumerate every bot, channel, or environment-job workflow
or every schedule; it does not destroy provider VMs, erase retained Temporal
histories, remove connector-local authentication files, or broadcast cache
eviction to other runtime replicas. Use the cleanup and verification steps
above to remove those remaining resources, and retain the records needed to
investigate a partial failure.


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
