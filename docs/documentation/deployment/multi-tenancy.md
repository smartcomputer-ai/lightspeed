# Managing universes

A universe has a permanent runtime UUID and, when managed through Platform,
belongs to one organization. Platform records who can use it. The runtime
stores and executes the work inside it. Keep the two records together when
backing up or recovering a universe.

The handle is the deployment-wide name you see in a universe URL, such as
`/app/u/research`. Use it to address the same logical universe in scripts
across environments. Use the UUID for recovery or to check a deletion, since
a handle can be changed or reused.

## Create a universe

Choose **New universe** in the universe switcher. You can create one where
you hold organization Creator or Admin access and the organization has room.
Platform asks for the organization if you can create in several. The creator
normally becomes the first Owner. A Platform Admin provisioning outside their
own organizations chooses an existing member as the Owner.

The name suggests a handle. To change it, go to **Settings → General →
Handle**, or use `lightspeed universe set-handle OLD NEW`. Update links and
scripts using the old handle after a rename. Use **Settings → Members** to
grant access to organization members, and see
[People and roles](../access-and-security/people-and-roles.md) for role choices.

For repeatable provisioning, use a [universe document](configuration-as-code.md).
The public Platform API also supports naming an initial Owner when creating a
universe for someone else.

## Keep Platform and runtime records aligned

A universe created directly on the runtime has no Platform organization. A
Platform Admin can find it under **Admin → Platform → Unclaimed Universes**.
Choose **Adopt**, then select the organization and its initial Owner. The
universe keeps its UUID, handle, and contents.

If a restore leaves a linked universe missing from the runtime, first check
that Platform is talking to the right runtime and database. A Platform Admin
can see the runtime status in the organization’s **Universes** list.
**Recreate on engine** starts the same universe empty. Restore sessions and
files from the original [recovery set](upgrades-and-recovery.md) when they
need to survive.

## Archive and delete a universe

Open the universe’s **Settings → General → Danger zone**.

- **Archive universe** hides it from the switcher. Existing links, API
  access, and running work continue. Use **Restore universe** to return it
  to the switcher.
- **Delete** blocks new work immediately, frees the handle, and starts
  permanent cleanup. Type the current handle to confirm. An Owner can
  delete an unprotected universe directly.

Use **Deletion protection and expiry** to control whether a universe can
be removed. An Owner can enable protection. Through Platform, only an
Organization Admin or Platform Admin can clear it, and the organization's
policy must allow that. Clear protection before deleting or setting expiry.
At its expiry time, an unprotected universe enters the same deletion process.
Activity does not extend the expiry.

After a Platform deletion request, check the organization's **Universes**
list for progress. Its quota slot becomes available after cleanup. For a
trusted direct runtime connection, the CLI can wait for cleanup.

```bash
lightspeed universe delete research --wait
```

For the direct runtime HTTP API, poll `GET /api/v1/universes/{universe}` by
UUID after requesting deletion. HTTP 410 (`gone`) marks completion. Platform
Admins and trusted runtime callers can inspect permanent records with
`lightspeed universe list --deleted`.

Cleanup removes the universe’s data, stops its workflows and bot schedules,
and asks environment providers to close provisioned machines. Its deletion
record identifies any machines whose closure the provider could not confirm.
Check those targets with the provider. Registered daemon host machines,
provider infrastructure shared by multiple environments, backups, retained
Temporal histories, and effects in external systems follow their own
retention or cleanup policies.

<a id="follow-a-request-into-its-universe"></a>
<a id="how-the-boundary-is-represented"></a>
<a id="what-the-deployment-shares"></a>
<a id="credentials-and-billing"></a>
<a id="machines-and-networks"></a>
<a id="access-inside-a-universe"></a>
<a id="evaluate-the-boundary-for-your-deployment"></a>

## Access and isolation

See [Tenant isolation and data protection](../access-and-security/tenant-isolation-and-data-protection.md)
for what a universe isolates and what the deployment shares.
