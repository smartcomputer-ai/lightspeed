# Sessions and runs

A session holds an agent's conversation and setup. A run is one attempt to
carry out a task in that session: it can contain several model turns and tool
calls before the agent finishes, fails, or is canceled. Sending another task
starts another run with the conversation that came before it.

This distinction gives you two kinds of control. You can ask for more work
after the current task, or change what the agent is doing now. The session
retains both the work and its history when you leave the page.

Use a Contributor, Operator or Admin account in the universe for the web
procedures below. To control an existing private session, you must be its
creator or an Admin; shared sessions can be controlled by Contributors and
above. If you haven't completed a task yet, start with
[Build your first agent](../getting-started/first-agent.md).

## Start and continue a session

Open **Sessions → New session**, enter a **Name**, and select a **Profile**.
Choose **Create** to use the saved profile. **Customize setup…** lets you
change the setup for this session without saving those changes back to the
profile. After customizing, choose **Create session**. You can also start
without a profile and configure the session directly.

Send a task in the composer. For the release editor from the first-agent
walkthrough, try:

```text
Read /workspace/changes.md and /workspace/release-notes.md.
Check every release-note claim against the change list.
Report any mismatch, but leave the files unchanged.
```

When the answer arrives, send a follow-up in the same session. The agent can
use the earlier conversation and its linked files. Starting a new session
from the same profile gives you a fresh conversation; workspace attachments may
still point to the same shared files.

## Share a session with the universe

New standalone sessions are private: through the Platform, only their creator
and Admins can read them. To share a review with the team, open the session
title menu, choose **Share with universe…**, and confirm with **Share**. This
also shares its sub-agents. Sharing cannot be undone.

Every member can then read the conversation, and Contributors and above can
continue or control it. Sharing and deletion remain creator-or-Admin actions
and require at least the Contributor role. Bot conversations are always
shared; delegated sessions follow their root's visibility.

Files written into an attached shared workspace remain visible through that
workspace even while the conversation is private. Direct core keys with the
`session` method group also have access to private sessions in their universe.
See [Private and shared work](../access-and-security/private-and-shared-work.md)
for the access model.

## Queue, steer, or stop work

Only one run is active in a session at a time. The composer changes its
behavior while that run is working:

| Action | Effect |
| --- | --- |
| Send while idle | Starts a run. |
| Send while a run is active | Queues a separate run after the current work. Queued runs execute in order. |
| **Cmd+Enter** on macOS, **Ctrl+Enter** elsewhere | Steers the active run with an additional instruction. |
| **Shift+Enter** | Adds a newline in the composer. |
| **Stop run** | Requests cancellation of the active run. |
| Cancel an item in the queued-runs bar | Removes that queued task without stopping the active one. |

Suppose the release editor is checking a long document. Queue
“After this, write a short summary” when that is a second task. Steer with
“Focus the current review on compatibility claims” when you want to change
the review already in progress.

Steering reaches the agent at its next model turn. It does not interrupt a
model response or tool call already executing. If the run is waiting for a
promise, steering waits with it; it does not wake the run by itself.

Cancellation stops further work and requests cancellation of active model and
tool activities. It does not undo effects that have already happened, such as
a saved file or a message sent by a tool. Wait for the run to reach its
canceled state before treating it as stopped. Other queued runs remain queued
and can start afterward, so cancel them separately if you want the session to
remain idle.

For environment commands, canceling a tool activity does not guarantee that
its remote operating-system process has stopped. Inspect and stop that
process explicitly as described in
[Processes and jobs](../environments/processes-and-jobs.md#collect-output-and-stop-a-process).

Some tools pause for approval. Review their arguments and choose **Approve**
or **Reject** for each pending call. The run continues once every pending
decision has been made. See [Tools and MCP](tools-and-mcp.md) for configuring
that policy.

## Inspect what happened

The transcript shows messages and tool activity. Each tool row names the
operation, its target, and how long it took. Open a row to inspect its
**Arguments**, **Result**, **Error**, and any reported **Effects**. For example,
after the agent says it saved a file, inspect the write result and open the
file to verify the change.

Images and documents appear as thumbnails and document links beside the
message or tool result that supplied them. Click one to open it at full size.
The agent can also reference those items inline in its answer.

When a run finishes, its thinking, tool calls, and interim notes fold behind
one strip that names the outcome and number of tool calls; the final reply
stays visible below it. Click the strip to open the run, or turn off
**Collapse completed runs** in the session title menu to keep runs open.
Events delivered to a bot appear as bands headed by their sender and kind.

![Expanded tool activity showing a completed search command, Arguments and Result tabs, and the matching file and line in its result.](../images/session-tool-result.png)

*Demo mode: an expanded tool call in “Fix flaky scheduler test.” The result
shows what the command found; **Arguments** shows the submitted request.*

Click a finished run's context and usage figures for a breakdown, or hide them
with **Show run statistics** in the session title menu. Context describes the
last model request; cumulative tokens describe the whole run. A missing
measurement means it was unavailable.

Long conversations load a recent window first. Scroll upward to load older
history. Context compaction can reduce what the model carries into future
calls while leaving the retained transcript available to inspect. It does
not mean that the agent will reproduce every earlier detail from memory; keep
important source material in files it can read again.

## Find and change a session

Open **Filter sessions** in the session list. Under **Include**, select
**Closed sessions**, **Sub-agent sessions**, or **Managed sessions** to show
those conversations. **Metadata filters** accept `key=value` pairs, and
**Metadata keys to show** adds useful values to the list.

Metadata is a descriptive map, for example `project=acorn` and
`purpose=release-review`. It does not grant access or instruct the model.
Open **Session settings** to edit it, custom instructions, model configuration,
and other setup, then choose **Apply setup**. Changes to the agent's working
setup require an open session with no active or queued runs.

Existing ordinary sessions keep the setup they received at creation. Editing
their source profile does not update them automatically. See
[Profiles and instructions](profiles-and-instructions.md) for explicit profile
application and the different behavior of bot conversations.

To start a fresh conversation with the same setup, create another session
from the same profile.

## Continue from the CLI

The CLI can open the same session as the web app. On Linux x86_64,
[download the prebuilt CLI](../deployment/self-hosting.md#download-standalone-binaries)
from the release matching your server. You can also build it from the repository:

```bash
cargo build --locked -p cli
```

The examples use `lightspeed` on your executable path. Use `./lightspeed`
for a release binary in the current directory, or `target/debug/lightspeed`
for the source build above.

For the authenticated development launcher stack from the quickstart, use its
runtime gateway and a universe API key created by an Admin. See
[API keys and service access](../access-and-security/api-keys-and-service-access.md)
for creating and revoking keys. A universe key already selects its universe:

```bash
export LIGHTSPEED_API_URL=http://127.0.0.1:18080/rpc
export LIGHTSPEED_API_KEY="<universe API key>"
unset LIGHTSPEED_UNIVERSE
lightspeed chat --session "<session-id>"
```

Copy **Session ID** from the web app's session title menu.
For a remote installation, use the gateway address and authentication supplied
by the operator. A deployment key needs `LIGHTSPEED_UNIVERSE` set to the core
universe UUID, distinct from the readable slug in the browser URL. Local
single mode accepts neither API keys nor universe headers.

Inside the terminal interface, `/help` lists commands. `/steer` sends an
instruction to the active run, and `/approve` or `/reject` decides a pending
approval by ID. `/interrupt` cancels the newest queued run first, or the active
run when no queued run exists. `/quit` exits the interface and leaves the
session available to reopen.

Applications can use `session/runs/start`, `session/runs/read`,
`session/runs/steer`, and `session/runs/cancel` from the
[API reference](../../../crates/api/contract/api-reference.md). Starting a run
acknowledges admission; follow its state and events to obtain the final result.
Use a stable `submissionId` when retrying the same submission.

## Close and retain a session

Leaving a browser tab or quitting the CLI has no effect on session lifecycle.
**Close session** is a separate, permanent action: a closed session cannot
accept more work or be reopened. Its retained history is still inspectable.
**Force close session** also cancels outstanding work, including queued runs.

Closed ordinary sessions can be deleted. **Also delete forks and delegated
children** includes their retention descendants, which must also be closed;
configuration-only clones are separate. **Delete after close (days)** sets
automatic retention, with a blank value keeping history until manual deletion.
The root session owns this policy for its retention tree, so descendants do
not independently choose how long that tree is kept.

Bot and channel conversations have a lifecycle controller. Their session
inspector identifies what manages them; use that controller's reset or close
actions. If you enable direct input in a managed-session inspector, you bypass
the controller's normal event admission, routing, and delivery policies. Use
the [bot conversation](bots-and-triggers.md) or connected chat for normal work.

## If a session behaves unexpectedly

| Symptom | What to check |
| --- | --- |
| A message is waiting while the agent works | It may be a queued run. Use steering for an instruction intended for the current task. |
| Steering has no immediate visible effect | The current model call or tool batch must finish before the next model turn can consume it. |
| Work starts again after stopping | Check for other queued runs. Stopping one run leaves those tasks in place. |
| Setup changes are refused | Wait for active work to finish and drain or cancel queued runs. Reload settings if another editor changed them. |
| A finished child or closed conversation is missing | Under **Filter sessions → Include**, select **Closed sessions** and **Sub-agent sessions**, or follow the child link from the parent transcript. |
| The agent lost a detail from much earlier | Inspect the retained history and restate the needed fact or point it to the source file. The current model context can be smaller than the transcript. |
