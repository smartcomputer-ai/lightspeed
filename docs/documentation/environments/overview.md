# Environments

An execution environment gives an agent a real filesystem and processes. The
agent's session continues to live in the Lightspeed runtime; the environment
supplies the operating system needed for work such as running tests, using a
command-line tool, or keeping a background experiment running.

This separation lets you choose compute according to the task. A session can
spend most of its time conversing, using MCP tools, or editing VFS files, and
use a machine when there is something to execute there.

## Two places for files

Lightspeed's VFS stores persistent files without an operating system attached.
An environment has its own ordinary filesystem. The two domains have separate
tools and separate contents:

| Domain | Where the files live | How the session gets access |
| --- | --- | --- |
| VFS workspace | Lightspeed's persistent storage | Workspace attachments in the session's VFS feature, each with `read` or `edit` access |
| Environment filesystem | The machine or container running the environment daemon | An environment attachment with its access level, and an active environment |

Suppose an agent writes a test plan in its VFS workspace, then selects a VM to
run the tests. The plan does not appear in the VM automatically. If a command
needs that file, it must be transferred explicitly. Identical paths in the two
domains can refer to unrelated files.

Prompt loading and skill discovery can use either domain when enabled in the
session's setup. Environment discovery reads the selected machine; VFS
discovery reads attached workspaces and snapshots. Copying a file between them
is still an explicit operation.

## How a machine becomes an environment

The `lightspeed-envd` daemon provides filesystem and process operations on the
machine. Lightspeed supports three ways to reach and manage it:

| Source | Connection and lifecycle | Typical use |
| --- | --- | --- |
| Registered | The daemon connects outward to the environment gateway using a registration key. You manage the underlying compute. | A workstation, VM, container, or pod behind NAT |
| External | Lightspeed connects to a daemon endpoint you register. You manage the underlying compute. | A reachable daemon on a protected network or the local development daemon |
| Provisioned | An environment provider creates and manages the machine and supplies access to its daemon. | VMs created through the included Incus provider |

A registered machine needs outbound connectivity to Lightspeed, so it does
not need its own inbound daemon address. A registration key admits machines
into a particular universe. Each admitted daemon then uses its own persistent
identity to reconnect.

A provider adds machine lifecycle management. Lightspeed asks it to create a
machine and, later, to close it; the provider runs everything in between,
including power, idle policy, and wake-up. The included Incus provider offers
operator-configured templates and runs VMs. A Platform admin registers the
provider and enables a binding for the universe; Operators and Admins can
then create environments from its available templates.

## Select an environment for a session

Environments belong to a universe. A session attaches the machines it may use
and selects one as its **Active environment**. Its file and process tools
operate on that machine. Each attachment sets an access level and optional
working directory; the levels are **Read files**, **Edit files**, **Run
commands**, and **Run durable jobs**, each including the preceding levels.

Enable **Environments** in the session setup, attach the machine, and select
it. A profile can also mark one attachment as its **Default environment**, which
fills an empty selection when that profile is applied. Contributors can use
existing environments in sessions they control; Operators and Admins manage
the machines and profiles. See [People and roles](../access-and-security/people-and-roles.md).

A profile attaches existing environments, can give every session a new
machine of its own from a provider template, and, for sub-agents, can
inherit the parent's active machine. For a provisioned environment, the
runtime waits for readiness before executing an environment-dependent tool
call. The session does not need to guess how long provisioning or a wake-up
takes.

**Environment selection tools** lets the agent switch among its attachments.
The agent keeps the same tools across switches, but each call must be allowed
by the active attachment. For example, a command tool may be available because
one attachment permits commands; it will still be refused on a machine
attached with **Read files** access.

Selecting an environment does not reserve it. Several sessions and bots can
use the same environment, and their processes and file writes share that
machine. Use separate environments when work needs separate operating-system
state. Changing the selection also does not move files or existing processes
to the new machine.

## Processes and background jobs

Process tools execute commands and return their output. A long-running
process can require subsequent interactions to collect output or stop it.
Background jobs provide another way to submit work and inspect its status and
results later, when the environment and its capabilities support it.

A job handle identifies its originating environment. Reading the job after
selecting a different machine still refers to the original job. Session
durability does not make an ordinary operating-system process survive the
destruction of its machine; provision and retain the environment according to
the work it is running.

## Power, disconnection, and cleanup

The provider owns a provisioned environment's power. Its template lists the
power states the machine supports (Incus offers running, paused, and
stopped) and a default idle policy, which lowers power or closes the machine
after periods of inactivity. A machine can be created with another policy.
Running processes, running jobs, and services a command left running keep
the machine awake. Using a paused or stopped environment wakes it, and the
runtime waits for it to become ready. Power buttons pass a one-off change to
the provider; see [Power and cleanup](power-and-cleanup.md).

Registered and external machines remain under your control. Connecting a
workstation does not give Lightspeed a way to power it on after shutdown.
A persistent registered environment stays open while its daemon is away, and
the daemon reconnects under the same identity; a tool call waits up to two
minutes for it. An ephemeral registered environment closes when its daemon
stops, or after its key's disconnect grace period when the connection is
lost.

Environments you create, register, or attach directly have their own
lifetime: they are credentialed, powered, and closed independently, and
closing or deleting a session leaves them available. A profile's **New per
session** attachment is the exception. It creates a machine for each session
and closes it when that session closes.

Closing a registered or external environment removes its availability in
Lightspeed; it does not delete or shut down your computer. Closing a provisioned
environment asks its provider to release the machine. If an active environment
disappears or closes, the session reports it as unavailable instead of silently
selecting another one.

## The daemon's access is the agent's access

Commands run as the operating-system user that runs `lightspeed-envd`. Choose
a dedicated user, container, or VM according to the access the task needs.
The daemon's default working directory is a convenience, not a process
sandbox. File-path restrictions and read-only file RPC settings do not prevent
a shell process from using the permissions of that OS user.

The daemon keeps its identity key, job records, and transfer journal in a
state directory outside the working directory. File and transfer tools can
never read or change that directory, but commands running as the daemon's
user can. Grant **Run commands** only where that is acceptable, or run
commands in a target whose daemon identity is not worth protecting, such as
a disposable VM.

Environment credential bindings can supply secrets to processes without
putting the values into the agent's instructions. Those processes can access
the injected credentials, so their permissions remain part of the environment's
trust boundary.

Start with [Bring your own compute](bring-your-own-compute.md) to connect a
machine, [Run registered daemons](registered-daemons.md) to run a fleet of
them as services, or [Incus VMs](incus-vms.md) to configure operator-managed
provisioning.
Then continue through the task guides:

- [Using environments](using-environments.md): create machines and select them
  through sessions and profiles.
- [Processes and jobs](processes-and-jobs.md): run a check, follow output, and
  manage work across run boundaries.
- [VFS transfer](vfs-transfer.md): move inputs to a machine and save outputs to
  persistent workspaces.
- [Environment credentials](credentials.md): assign secrets to commands and
  understand their resolution and lifetime.
- [Power and cleanup](power-and-cleanup.md): pause, wake, retain, and close
  environments according to their source and ownership, and read their
  state and status.
- [Networking and ingress](networking-and-ingress.md): connect daemons and
  publish applications through the supported provider edge.

The [environment protocol reference](../reference/environment-protocol.md) and
[environment-variable reference](../reference/environment-variables.md#environment-services)
define the interfaces and settings.
