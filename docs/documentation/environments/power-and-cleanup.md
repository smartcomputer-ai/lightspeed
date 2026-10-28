# Power and cleanup

A session's lifetime and a machine's lifetime are separate. You can leave a
session available while its provisioned VM is paused, share a running machine
between sessions, or close a disposable machine when its task ends. The
environment's source determines which operations are available and what they
remove.

Use an Operator or Admin account, then inspect the machine under
**Environments → Details**. Its **Source** determines whether a provider
manages the machine or Lightspeed only holds its connection. Check what the
provider reports and its idle policy before changing a machine that other
sessions may also use.

## Know who owns power

A provider owns the power of the machines it creates. It applies its own
idle policy, wakes a machine when work needs it, and may change power on its
own schedule. Lightspeed asks for a change only when you press a power
button or call the power API, and it never reverts a change the provider
made. What Lightspeed shows is the provider's latest report, refreshed when
you change power and otherwise about every five minutes.

Registered and external environments are machines you manage. Lightspeed can
close their records, but it cannot power them on or off or delete their
files.

## Distinguish power from closure

| Operation | Effect on an Incus VM | What the next use needs |
| --- | --- | --- |
| Pause | Freezes execution while retaining memory and disk. | Nothing: using it resumes the machine. |
| Stop | Powers off while retaining disk. Running processes end. | Nothing: using it boots the machine again, which takes longer. |
| Close | Releases the VM and its disk. The environment identity is terminal. | Create and select another environment. |

The included Incus provider supports running, paused, and stopped power
states. The protocol also supports a suspended state for providers that save
execution state to disk, but Incus does not offer it. The UI shows the
power buttons the environment's template lists.

## Pause, resume, and stop a provisioned environment

For a disposable test VM from [Incus VMs](incus-vms.md), save any needed
output, then expand its **Details** and choose **Pause**. Lightspeed asks the
provider at once and shows the state it reports. **Resume** asks for
running again.

You can also leave it paused and ask a session using it to run `pwd`. The
tool's connection to the machine wakes it, and the tool waits until it is
ready. Selecting the environment leaves its power unchanged, and ordinary
model conversation and VFS work do not need the machine to wake.

**Stop** retains the disk but ends execution. On the next use, the VM boots
again and starts its daemon. Job records can survive on the disk, but
unfinished daemon jobs become interrupted; stopping a VM does not preserve
an ordinary process for later continuation.

Manual power controls do not check for running work. Pausing can freeze
active work, and stopping can terminate it. Inspect running tasks before
choosing the action. The provider stays in charge afterwards: its idle
policy can pause or stop the machine again later, and the next use wakes a
machine you stopped by hand.

With the [CLI connection settings](../using-lightspeed/sessions-and-runs.md#continue-from-the-cli)
configured, the same requests are:

```bash
target/debug/lightspeed environment power "<environment-id>" paused
target/debug/lightspeed environment power "<environment-id>" running
```

Use `stopped` instead of `paused` to request a stop. Each request waits for
the provider's answer and prints the environment with the state the provider
reported, which can still be a transition. A request fails when the provider
cannot be reached or does not offer that state; nothing is queued for later.

## Interpret state and status

Every environment has a lifecycle **state**:

| State | Meaning |
| --- | --- |
| Provisioning | A provider is creating the machine, and Lightspeed has not yet completed its first connection to the daemon. Registered and external environments skip this state. |
| Open | The environment can be used. |
| Closing | A provisioned machine is being released. |
| Closed | The environment can no longer be used. Its close reason says why. |
| Failed | A provisioned machine could not be created or broke. Its state message says why. |

The **status** badge combines the state with what is known about the
machine:

| Status | Meaning for the next operation |
| --- | --- |
| Provisioning | The machine is still being created. Session tools wait for it. |
| Ready | The environment is open and, as far as Lightspeed knows, running or connected. |
| Paused, suspended, or stopped | The provider reported a powered-down machine. The next tool call wakes it and waits. |
| Disconnected | A registered daemon is not connected. A tool waits up to two minutes for a persistent daemon to return; an ephemeral one is not waited for. |
| Unknown | The provider could not tell, or reported something unexpected. Inspect the provider. |
| Closing or closed | Cleanup is in progress or finished. Do not submit new work. |
| Failed | Inspect the state message and the provider or gateway diagnostics. |

The details show the state message while an environment is provisioning,
closing, or failed, and the close reason once it is closed. Provider trouble
does not fail an environment at once. While a provider is unreachable or
answers with a temporary error, Lightspeed retries, with growing pauses of up
to five minutes, and shows the latest problem. An environment that has not
opened within 30 minutes of its creation fails with that problem. An
environment also fails when the provider reports its machine broken, or when
its daemon speaks a different environment protocol version than the runtime;
the message names both versions. A machine the provider removed on its own,
for example after its idle close threshold, closes the environment with the
reason **Ended by the provider**.

A session can retain the ID of an environment that has closed or become
unavailable. The runtime reports that failure; it does not silently select a
replacement. Update the session or profile explicitly when choosing another
machine.

Standalone API calls can receive `environment_not_ready` while a machine
provisions, wakes, or reconnects. Follow the retry rules for the operation
rather than blindly resubmitting new work;
[Processes and jobs](processes-and-jobs.md#start-jobs-through-the-api) covers job
creation identities.

## Set an idle policy

The provider runs idle policy. A policy names how long a machine may stay
idle before the provider pauses, suspends, stops, or closes it. Each stage
is optional, and the thresholds must not decrease in that order. The
template names a default, shown in **New environment** when you choose the
template and in the environment's **Details**. An operator sets those
defaults in the provider's configuration; see
[Incus VMs](incus-vms.md#configure-and-start-the-provider).

A machine keeps the policy it was created with. To create one with another
policy, pass `idle` to `environments/create`:

```json
{"requestId": "acorn-build-1", "providerId": "incus-local", "templateId": "dev-small-v1", "idle": {"pauseAfterMs": 600000, "stopAfterMs": 14400000}}
```

There is no way to change the policy of an existing machine. Create a
replacement, or use the power buttons for a one-off change.

Idle time comes from daemon activity, not time since the last chat message.
Filesystem, process, and job operations reset it. Running processes, running
jobs, and services a command started and left running keep the machine
awake. Initialization, idle polling, and the prompt and skill discovery that
runs before each agent run do not count as use, so a chat-only session does
not hold an idle machine awake. A provider also never powers a machine down
while a connection to its daemon is open.

The provider checks periodically, so a threshold is not an exact wall-clock
deadline. The included Incus provider checks every minute by default and
keeps escalating after a pause: a policy that pauses after 10 minutes and
stops after 60 stops the machine about 50 minutes after pausing it. To
verify a policy, leave the VM without running work and without calls that
keep it active, then watch its status. Another session using the same
environment can keep it busy or wake it again.

Public HTTP traffic to an application does not count as daemon activity and
does not wake a sleeping VM. Choose a power policy compatible with the
application's availability needs; see
[Networking and ingress](networking-and-ingress.md).

## Choose who owns cleanup

An environment you create under **Environments → New environment** has its
own lifetime. Configure its credentials on the Environments page and close it
explicitly when finished. Closing or deleting a session or bot that attached
it leaves it open. Profiles only attach existing environments or, for
sub-agents, inherit a parent's selection.

Session-owned job promises are canceled when the session closes. Standalone
API jobs have no session promise, and ordinary remote processes may outlive
agent cancellation. Those lifetimes are explained in
[Processes and jobs](processes-and-jobs.md); a session close alone is not a
universal operating-system cleanup command.

Closed provisioned environments disappear from Lightspeed seven days after
they close. Closed registered and external environments stay listed under
**Show closed environments**.

## Close a machine deliberately

Before closing a provisioned environment, save required output to durable
storage outside it, such as a VFS workspace. Inspect other sessions and bots
that name it, and choose replacements where they still need compute.

Use **Close environment** in its details, for any source. The CLI equivalent
is:

```bash
target/debug/lightspeed environment close "<environment-id>"
target/debug/lightspeed environment read "<environment-id>"
```

Closing asks the provider to destroy everything the environment's machine
holds, public ingress included. For Incus, that is a guest shutdown of up to
30 seconds, then a forced stop, then deletion of the VM and disk. The
operation is asynchronous. Lightspeed closes by the environment's identity,
so a machine whose creation failed midway is still found and removed. A
provider error leaves it closing while Lightspeed retries, so inspect the
final state and provider diagnostics when cleanup does not finish.

Closing a registered or external environment takes effect at once and
removes access through Lightspeed while leaving the computer and files under
your control. A connected registered daemon is told its environment closed;
it exits with status 4, or parks until it is stopped when it runs with
`--on-closed=park`. An external passive daemon is not terminated by the
close. Neither path is a comprehensive cleanup of programs started on the
machine. Inspect and manage those processes and files explicitly.

Environment closure does not delete the independent VFS workspace or revoke
the universe credentials assigned to the machine. It also does not retain
machine files just because a session's transcript still describes them.

## Handle borrowed-machine disconnects

A persistent registered environment stays open while its daemon is away.
Restart the daemon with the same retained state directory to reconnect under
the same identity. A tool call that arrives meanwhile waits up to two minutes
for the daemon to return, which covers an ordinary restart or rollout, then
fails. Lightspeed cannot switch on that machine for you.

The registration key decides how long an absent daemon's environment
survives. An ephemeral key closes an environment as soon as its daemon stops
deliberately, and after its disconnect grace period, five minutes by default,
when the connection is simply lost. Tools do not wait for an ephemeral
daemon. A persistent key can set **Close when offline for**, which closes an
environment whose daemon stays away that long; without it, the environment
stays open until someone closes it. Once closed, a daemon identity is spent. A replacement
needs a fresh state directory and a valid registration key.

Revoking a registration key blocks new admissions. Existing identities can
continue reconnecting unless their environments are also closed. The key
revoke operation can optionally close those environments; that still leaves
the borrowed computers under your control. The full connection procedure is
in [Bring your own compute](bring-your-own-compute.md), and service
deployments are in [Run registered daemons](registered-daemons.md).

## If cleanup or wake-up is unexpected

| Symptom | What to check |
| --- | --- |
| The VM never pauses | Check the idle policy in its details, running processes and jobs, services a command left running, and calls from other sessions. |
| An application goes offline despite receiving traffic | App traffic does not reset daemon idle time or wake the VM. Review the template's idle policy. |
| A VM paused by hand is running again | A tool call or another session used it, and the provider woke it. |
| An environment disappears while a session uses it | Inspect its close reason: an idle close by the provider, an explicit close, its session closing, or the registration key's policy. |
| A borrowed machine cannot resume through Lightspeed | Restart its daemon or machine directly; Lightspeed has no power control over it. |
| A provisioned environment remains closing | Inspect provider reachability and deletion errors in its state message. A recorded close request does not prove the VM was removed. |

The [API reference](../../../crates/api/contract/api-reference.md) defines
`environments/power/put`, `environments/create`, and the environment
read and close operations.
