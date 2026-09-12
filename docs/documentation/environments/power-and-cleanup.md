# Power and cleanup

A session's lifetime and a machine's lifetime are separate. You can leave a
session available while its provisioned VM is paused, share a running machine
between sessions, or close a disposable machine when its task ends. The
environment's source and cleanup policy determine which operations are
available and what they remove.

Start by inspecting **Environments → Details** for the machine. Check its
source and whether it was **Provisioned for session**. Selecting an existing
environment does not change an earlier ownership or retention policy.

## Distinguish power from closure

| Operation | Effect on an Incus VM | What the next use needs |
| --- | --- | --- |
| Pause | Freezes execution while retaining memory and disk. | Resume the existing machine. |
| Stop | Powers off while retaining disk. Running processes end. | Boot the machine again. |
| Close | Releases the VM and its disk. The environment identity is terminal. | Create and select another environment. |

The included Incus provider supports running, paused, and stopped power states.
The protocol also supports suspended state for providers that save execution
state to disk, but Incus does not currently offer that state through Lightspeed.
The UI displays controls according to the provider's advertised capabilities.

Registered and external environments are machines you manage. Lightspeed can
close their logical records, but it cannot power them down or delete their
files through provider lifecycle controls.

## Pause, resume, and stop a provisioned environment

For a disposable test VM from [Incus VMs](incus-vms.md), save any needed output,
then expand its **Details** and choose **Pause**. Wait for the observed status
to become paused. **Resume** requests running again.

You can also leave it paused and ask a session using it to run `pwd`. Using
a sleeping provisioned environment with power support requests a wake-up;
selecting it leaves its power state unchanged. The environment-dependent tool waits for readiness before
executing. Ordinary model conversation and VFS work do not need that machine
to wake.

**Stop** retains the disk but ends execution. On the next use, the VM boots
again and starts its daemon. Job records can survive on the disk, but unfinished
daemon jobs become interrupted; stopping a VM does not preserve an ordinary
process for later continuation.

Manual power controls do not check the idle reaper's “no running work” condition.
Pausing can freeze active work, and stopping can terminate it. Inspect running
tasks before choosing the action.

With the [CLI connection settings](../using-lightspeed/sessions-and-runs.md#continue-from-the-cli)
configured, the same power requests are:

```bash
target/debug/lightspeed env power "<environment-id>" paused
target/debug/lightspeed env read "<environment-id>"
target/debug/lightspeed env power "<environment-id>" running
```

Use `stopped` instead of `paused` to request a stop. Each request records desired
power; it does not wait for every provider action to finish. Read the environment
again to observe convergence.

## Interpret readiness and status

Desired power is Lightspeed's request. The status is its current observation
of the environment. During a transition, they can differ.

| Status | Meaning for the next operation |
| --- | --- |
| Provisioning or booting | The machine or daemon is still becoming available. Session environment tools can wait for readiness. |
| Ready | The environment is available, subject to its advertised capabilities and current connectivity. |
| Paused or suspended | A supported provisioned environment needs to resume before use. |
| Offline | A provisioned machine may be stopped; a borrowed machine may have lost its daemon connection. The source determines whether Lightspeed can wake it. |
| Closing | Cleanup is in progress. Do not submit new work. |
| Closed | This environment can no longer be used. |
| Failed or unknown | Inspect provider or gateway diagnostics instead of assuming readiness. |

A session can retain the ID of an environment that has closed or become
unavailable. The runtime reports that failure; it does not silently select a
replacement. Update the session or profile explicitly when choosing another
machine.

Standalone API calls can encounter a not-ready response during wake-up. Follow
the retry rules for the operation rather than blindly resubmitting new work;
[Processes and jobs](processes-and-jobs.md#start-jobs-through-the-api) covers job
creation identities.

## Set an idle policy

Open a provisioned environment's **Details → Idle policy…**. The fields
**Pause after**, **Suspend after**, **Stop after**, and **Close after** use
minutes. A blank field omits that stage. There are no automatic default
thresholds: an environment without an idle policy remains available until
another lifecycle action changes it.

For a development VM you want to keep but avoid leaving active, set only
**Pause after** to `10` and leave the other fields empty. Save the policy.
The CLI equivalent is:

```bash
target/debug/lightspeed env idle-policy "<environment-id>" --pause-after-min 10
```

The command replaces the entire policy. To remove automatic idle action:

```bash
target/debug/lightspeed env idle-policy "<environment-id>" --clear
```

Idle time comes from daemon activity, not time since the last chat message.
Filesystem, process, and job operations affect that activity clock. Running
processes and jobs prevent automatic idle action. Initialization and idle
polling do not keep the machine busy by themselves.

The runtime checks periodically, so a threshold is not an exact wall-clock
deadline. To verify the policy, leave the VM without tracked work and without
calls that keep it active, then observe its status. Another session using the
same environment can keep it busy or wake it again.

## Understand the current staging limit

Thresholds must be non-decreasing in the order pause, suspend, stop, close.
At a poll, the reaper chooses the most escalated supported stage already due.
It only evaluates environments that are ready and still have desired power
set to running.

Once a policy pauses or stops the machine, later stages do not continue
escalating automatically while it remains powered down. A policy with pause
after 10 minutes and stop after 60 therefore does not promise a stop 50
minutes after pausing. Use one automatic stage when that is the behavior you
need, or manage the later lifecycle action explicitly.

Idle detection also has a process boundary. A long-lived root process or job
counts as running work. A command that exits after leaving a child service
behind no longer keeps the daemon busy on that child's behalf. The machine
can then be paused or closed by its idle policy while that service is useful.

Public HTTP traffic to an application does not pass through daemon activity
accounting and does not wake a sleeping VM. Choose a power policy compatible
with the application's availability needs; see
[Networking and ingress](networking-and-ingress.md).

## Choose who owns cleanup

Every environment has a lifetime managed independently of sessions. Create
machines through **New environment**, configure their credentials and idle
policy on the Environments page, and close them explicitly when finished.
Closing or deleting a session or bot never closes its selected environment.
Profiles only select an existing environment or inherit a parent's selection.

Session-owned job promises are canceled when the session closes. Standalone
API jobs have no session promise, and ordinary remote processes may outlive
agent cancellation. Those lifetimes are explained in
[Processes and jobs](processes-and-jobs.md); a session close alone is not a
universal operating-system cleanup command.

## Close a machine deliberately

Before closing a provisioned environment, save required output to durable
storage outside it, such as a VFS workspace. Inspect other sessions and bots
that name it, and choose replacements where they still need compute.

On a provisioned environment, use **Close environment** in its details.
The CLI works for all source types:

```bash
target/debug/lightspeed env close "<environment-id>"
target/debug/lightspeed env read "<environment-id>"
```

For Incus, closing disables ingress, requests guest shutdown, and deletes the
VM and disk. The operation is asynchronous. A provider error can leave it
closing while reconciliation retries, so inspect the final status and provider
diagnostics when cleanup does not finish.

For an individual registered or external environment, use the CLI or
`environments/close` API; the current web app does not expose the individual
close button for those source types. Closing their records removes access
through Lightspeed while leaving the computer and files under your control.
An outbound registered daemon exits when it receives the terminal rejection
for its closed identity; an external passive daemon is not terminated by the
logical close. Neither path is a comprehensive cleanup of programs started
on the machine. Inspect and manage those processes and files explicitly.

Environment closure does not delete the independent VFS workspace or revoke
the universe integrations used by its credentials. It also does not retain
machine files just because a session's transcript still describes them.

## Handle borrowed-machine disconnects

A persistent registered environment remains offline while its daemon is away.
Restart it with the same retained state directory to reconnect under the same
identity. Lightspeed cannot switch on that machine for you.

An ephemeral registered environment closes after its configured disconnect
grace period, which defaults to five minutes. Once closed, its daemon identity
is spent. A replacement needs a fresh state directory and a valid registration
key. The grace period concerns disconnection, not inactivity while connected.

Revoking a registration key blocks new admissions. Existing identities can
continue reconnecting unless their environments are also closed. The key
revoke operation can optionally close those environments; that still leaves
the borrowed computers under your control. The full connection procedure is
in [Bring your own compute](bring-your-own-compute.md).

## If cleanup or wake-up is unexpected

| Symptom | What to check |
| --- | --- |
| The VM never pauses | Check the idle policy, tracked running work, and calls from other sessions. |
| A paused VM never reaches its later stop threshold | Later stages do not escalate while powered down. Choose a single stage or perform the later action explicitly. |
| An application goes offline despite receiving traffic | App traffic does not reset daemon idle time or wake the VM. Review its power policy. |
| An environment disappears while a session uses it | Inspect its idle close policy, explicit close requests, and provider lifecycle. Session closure never closes the environment. |
| A borrowed machine cannot resume through Lightspeed | Restart its daemon or machine directly; it has no provider power control. |
| A provisioned environment remains closing | Inspect provider reachability and deletion errors. A recorded close request does not prove the VM was removed. |

The [API reference](../../../crates/api/contract/api-reference.md) defines
`environments/power/put`, `environments/idle-policy/put`, and the environment
read/close operations.
