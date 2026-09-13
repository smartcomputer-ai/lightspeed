# Using environments

An environment gives a session access to a machine's files and processes.
The session selects one environment at a time, while the environment remains
a resource in the universe. Several sessions can select the same machine;
selection does not reserve it or create a private copy.

Selection checks that the environment is attached in the session's
configuration and is not failed, closing, or closed. It does not connect to the machine or change
its power state. Sleeping, offline, and starting environments can be selected;
tools check readiness and request wake-up where supported when they use it.
Selecting the same environment again rechecks its current registry state.

This guide starts with a machine that already appears under **Environments**.
Use [Bring your own compute](bring-your-own-compute.md) to connect one you
control, or [Incus VMs](incus-vms.md) to configure provider-managed machines.
The [environment overview](overview.md) explains the available sources and
the separate VFS and machine filesystems.

Use a universe owner/admin or platform administrator account for these web
procedures. You also need a session with a working model.

## Select a machine for a task

Open **Environments**, find the machine, and expand **Details**. Check its
source, status, and **Environment ID**. A display name helps you find it;
the ID is the stable value profiles and API calls reference.

![CI runner environment with Details expanded, showing its ready status, environment ID, provisioned source, running power state, idle policy, and controls.](../images/environment-details.png)

*The demo's CI runner is a provisioned machine. Its **Environment ID** is the
value to select; its status and power state describe the machine's current state.*

Open the session you want to use. With no active or queued runs, choose
**Session settings**, enable **Environments**, add the machine under
**Environments**, and select it under **Active environment**. Choose **Apply
setup**.

Each attached environment carries its own **Access**: **Read**, **Edit**,
**Exec**, or **Jobs**, each including the levels before it. Enabling
**Environments** in the profile or session editor also turns on **Prompt
loading** and **Skill discovery**; adjust each independently. Existing
configurations keep their saved settings; a machine that is not attached
cannot be selected.

**Environment selection tools**, below **Skill discovery**, remains off by
default. For this first check, give the attachment **Exec** access; that is
sufficient to run a simple command on the machine you selected without
granting durable jobs.

Send:

```text
Inspect the active environment, then run pwd there and report the working
directory. Do not create or change files.
```

Inspect the tool result, including any process error. On the machine from the
bring-your-own-compute walkthrough, the working directory should be the
`workspace` directory passed to the daemon. A successful reply in chat is
useful, but the actual process output verifies where the command ran.

The environment's default working directory belongs to the daemon setup.
Commands can choose a different working directory. Neither setting confines
the process to that directory; it runs with the operating-system permissions
of the daemon user.

## Choose the access level

A session can attach several environments, each with its own **Access**. The
levels form a ladder: `read` exposes reading, listing, search, and glob tools;
`edit` adds write, edit, and patch tools; `exec` adds starting and continuing
processes; `jobs` adds workflow-backed durable jobs. Each level includes the
ones before it. Read-only files with command execution is deliberately not
expressible, because a process can write files regardless of the file tools.
Attach with the lowest level that covers the task.

```json
{"features":{"environments":{"environments":[{"environmentId":"<environment-id>","default":true,"access":"read"}]}}}
```

The list is the allowed set: the session can select, read, and run only on a
listed machine. The tools the agent sees are the union of every attachment's
access, installed once; a call that the active machine's access does not
cover is refused when it executes, with that machine's access named.
Switching machines therefore never changes the toolset. Access is a tool
grant, subject to the endpoint's capabilities and operating-system
permissions. Prompt loading and skill discovery are authorized separately by
their own blocks, and any attachment lets the agent read discovered skills.
Selection and environment status remain separate.

The session's context includes an **Environment catalog** listing every
attachment with its display name, status, access, working directory, and
which one is active, so the agent knows what it may use before calling a
tool. After a switch, the old catalog is removed until the next idle refresh.
Use `environment_list` or `environment_read` for current status during a run.

## Keep files in the right domain

Suppose the session also links the release-notes VFS workspace at `/workspace`.
That path belongs to the session's VFS view. It does not place the files in a
directory named `/workspace` on the selected machine.

To check VFS content with a command, use `vfs_materialize` to copy the selected
file or tree to an exact environment destination. Use `vfs_capture` to save
machine outputs into a writable VFS workspace. Both replace the complete
selected destination by default, preserving siblings. See
[VFS transfer](vfs-transfer.md) for partial copies, content reuse, large files,
and workspace revision conflicts.

After transfer, the copies can diverge. Editing the machine's copy does not
update VFS, and changing the active environment does not move either copy.
Use explicit source and destination paths in tasks that cross this boundary.

## Reuse a machine through a profile

In **Profiles**, open the profile and enable **Environments**. Add the
machine under **Environments** with the access the job needs and mark it as
the **Default** attachment. Save the profile and start a new session from it.

Every session created from this setup activates that machine. The default
fills an empty active pointer whenever the profile is applied, creation
included, and never overrides a live selection: applying the profile to an
existing session first clears an active environment the profile no longer
lists, then activates the default only if nothing is active. At most one
attachment can be the default. This is useful for a shared repository
checkout, a long-lived service, or several bot conversations working with the
same operating-system state. The profile does not close the environment when
one of those sessions ends.

Sharing also means sharing file changes, processes, installed tools, and
environment-bound credentials. Lightspeed does not coordinate edits between
the sessions. Use separate directories for independent tasks and separate
environments when they need separate machine state or credential access.

Prefer a persistent registered environment for a profile that names a
borrowed machine. An ephemeral registration closes after its disconnect
grace period; once closed, that identity cannot return and the profile's
saved selection becomes unavailable.

## Create an independent machine

Use **Environments → New environment**. Select a **Template**, enter a
**Display name**, configure an idle policy if needed, and choose **Provision**.
The button appears when the universe has a non-deprecated template from an
enabled binding. Configure [credentials](credentials.md) on the environment,
then select it in a session or profile.

The environment can be selected while it is provisioning or booting. Tools
wait for readiness before dispatching. If provisioning fails, inspect the
provider and explicitly create or select a replacement. Session creation and
profile application never allocate machines.

Environments remain available when sessions close or are deleted. Manage
cleanup through explicit environment operations and idle policies; see
[Power and cleanup](power-and-cleanup.md).

## Use environments with bots and sub-agents

For a bot whose Main conversation, routed threads, and chat conversations
should use one machine, attach the same environment as the **Default** in its
profile. Resetting a conversation leaves that environment intact for the
successor and other sessions using it. Execution polls that name no
environment run on the profile's default attachment; because a live selection
is never overridden, a bot conversation that switched to another attached
machine can diverge from its polls. Name the environment on the poll when
they must match.

A child profile can attach **Inherit the parent's active environment
(sub-agents only)** (`"inherit": true`) with its own access level. The
attachment resolves to the parent's active machine when the child is spawned;
the child shares that machine without copying it and does not close it as its
own resource. If the parent has no active environment, the inherit attachment
is dropped; if the child also attaches the same machine explicitly, the
explicit attachment wins. An `inherit` attachment in a standalone session or
a plain session configuration is rejected.

The child can also attach a different existing machine. Create any new machine
through the environment API first. Its VFS links remain independent of these choices. See
[Sub-agents and federation](../using-lightspeed/subagents-and-federation.md)
for the rest of the child-profile boundary.

## Grant model-driven selection carefully

Enable **Environment selection tools** (`"selection": true`) when the model
should list, activate, and deactivate attached environments itself. Without
that switch, you can still select a machine through the client or profile,
and the model can still read its active environment with `environment_read`.
The switch does not grant environment provisioning and does not widen the
allowed set: `environment_list` lists the attachments, and `environment_read`
and `environment_activate` accept only attached ids. Their results carry the
attachment's access line.

Session configuration has no provider or registration-key filters. Which
machines a session may use is exactly its attachment list; registration keys
remain an operator concept for enrolling machines.

The runtime also rejects an ambiguous tool batch that changes selection and
uses the selected environment in the same batch. The agent must select first,
then perform the file or process operation in a later batch. Existing job
handles keep their original environment even after selection changes.

## Change or clear the selection

In an idle session's settings, choose another attached **Active environment**,
or choose **No active environment**, then **Apply setup**. This changes the
reference; it does not migrate files, terminate existing jobs, or close the
machine.

A plain configuration replacement (`session/config/put`) never activates a
default; it only clears an active environment that the new configuration no
longer lists, so a deliberate deactivation is not undone by a configuration
edit. Applying a profile fills an empty pointer from the profile's default
attachment and leaves a live, still-listed selection alone. Creating a session
with `session/start` can name an attached environment or `none` to suppress
the default.

With the [CLI connection settings](../using-lightspeed/sessions-and-runs.md#continue-from-the-cli)
configured, the equivalent controls are:

```bash
target/debug/lightspeed env list
target/debug/lightspeed env read "<environment-id>"
target/debug/lightspeed env activate --session "<session-id>" "<environment-id>"
target/debug/lightspeed env deactivate --session "<session-id>"
```

The public methods are `environments/list`, `environments/read`,
`session/environments/activate`, and `session/environments/deactivate` in the
[API reference](../../../crates/api/contract/api-reference.md). Activation
accepts only an environment attached in the session's configuration.

## If the machine is unavailable

| Symptom | What to check |
| --- | --- |
| **New environment** is missing | Check the universe binding, provider templates, and whether the template is deprecated. Borrowed machines use registration or attachment instead. |
| Selection succeeds but the first tool waits | A provisioned machine may still be booting or waking. Inspect its status and provider health. |
| A registered machine is offline | Restart or reconnect its daemon using the retained identity. Lightspeed cannot power on that borrowed machine. |
| A visible environment is rejected by the session | Check that it is attached in the session's configuration, plus the machine's capabilities and lifecycle status. |
| A file, process, or job call is refused on the active machine | The active attachment's access does not cover it. The toolset is the union of all attachments; raise that attachment's access or switch to one that covers the call. |
| A saved profile points to a closed machine | Select a replacement explicitly. The runtime does not silently switch to another environment. |
| A VFS file is missing on the machine | Transfer it explicitly and check which filesystem each tool used. |
