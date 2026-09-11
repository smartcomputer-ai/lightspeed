# Processes and jobs

Process tools run commands in the active environment and let the agent collect
output or continue an interactive process. Jobs add persisted execution
records, dependencies, and workflow supervision for work the agent needs to
follow over time. Both execute on the environment machine, with that machine's
filesystem and operating-system permissions.

The normal web workflow is to ask an agent in a session. The Environments page
does not contain a terminal or a jobs console, and the user CLI's `env`
commands manage environments rather than execute commands. You can use the
CLI's chat interface with the same session and capabilities.

Start with [an active environment](using-environments.md) and a model that
can call its process tools. Enable **Command execution** under **Environments**
in the profile or session setup (`features.environments.commands: true`). This
is independent of **Durable jobs**, which keeps its own grant. The first exercise needs a POSIX shell and common
utilities such as `grep`. The example Incus image supplies them.

## Run a check against a file

Use the release notes from [Build your first agent](../getting-started/first-agent.md).
Set environment **File tools** to **Edit files** for this exercise, then ask the
agent to prepare a separate machine copy:

```text
Create a new, uniquely named task directory under the active environment's
working directory. Read /workspace/release-notes.md with the VFS tools and
copy its exact contents into release-notes.md in that task directory using
environment tools. Read the machine copy back to verify the transfer.
Report the absolute task directory. Leave the VFS original unchanged.
```

Next, ask the agent to write this script as `check-release.sh` in that
directory and run it there with `sh check-release.sh`:

```bash
set -eu
test -s release-notes.md
for title in "New" "Fixed" "What to expect"; do
  if ! grep -Eq "^#{1,6}[[:space:]]+$title[[:space:]]*$" release-notes.md; then
    printf 'Missing heading: %s\n' "$title" >&2
    exit 1
  fi
done
printf 'Release-note structure passed\n'
```

Inspect the command, working directory, output, and exit status in the
transcript. This check verifies that the file is nonempty and has the expected
headings. It does not verify the release claims; the reviewer's source-based
review still serves that purpose.

The file and script remain on the machine after the command ends. Ask the
agent to read them in another run to verify that persistence. Save any result
you need beyond the environment's lifetime back into VFS explicitly.

## Understand the process controls

The visible tool names follow the selected model API. They are different
presentations of the environment process service:

| Model API | Start | Continue or read output | Stop |
| --- | --- | --- | --- |
| OpenAI Responses | `exec_command` | `write_stdin` | Send an interrupt through `write_stdin`. |
| Anthropic Messages | `Bash` | `BashOutput` | `KillShell`. |
| Chat Completions | `run_process` | `continue_process` | Supply an interrupt or kill signal to `continue_process`. |

For example, these are model-facing `run_process` arguments for the script
above. Replace the directory with the absolute path created by the agent:

```json
{
  "argv": ["sh", "check-release.sh"],
  "cwd": "<absolute task directory>",
  "yield_ms": 1000,
  "timeout_ms": 60000
}
```

`argv` is an argument array. It does not interpret pipes, redirection, or other
shell syntax by itself; use a shell explicitly when the command needs them.
The working directory is on the environment machine. The session's
`features.environments.workingDirectory` supplies the default for file tools,
commands, jobs, and prompt/skill discovery; when unset, the endpoint's default
is used. A per-command `cwd` overrides this base and relative overrides resolve
against it. A shell `cd` does not persist into later tool calls. VFS has a
separate `features.vfs.workingDirectory`, defaulting to `/`.

The wait before yielding and the command's deadline are separate. In this
example, the tool waits up to one second before returning available output
and a running handle. The command has a 60-second kill timeout. A returned
`running` status means the command still needs to be followed; it is not a
successful exit.

Defaults differ by surface. Anthropic's foreground `Bash` uses a 60-second
kill timeout by default; its background mode returns immediately and does
not apply that timeout. Other process requests can have no kill timeout when
none is supplied. State the desired timeout in the task when it matters.

For interactive input, the canonical and Responses tools can start a process
with `tty: true` and send later input through its handle. Plain-pipe processes
receive their one-shot input at start and then EOF. Anthropic's current
process surface exposes no PTY or stdin controls; use noninteractive commands
or input files on that route.

## Collect output and stop a process

Follow-up calls return output through a daemon-owned cursor. Reconnecting a
client while the daemon remains alive does not require restarting the
command. Use the handle returned by the start call, and keep the session on
the originating environment when continuing it.

Output is bounded. The daemon can omit the middle of accumulated output and
reports omitted bytes; important complete logs should be written to a file.
The included daemon retains finished process handles for 60 seconds after the
exit is first observed. They are not a substitute for stored job records or
saved artifacts.

Ask the agent to stop the specific process when it is no longer needed. An
interrupt sends a signal the command can handle or ignore; a kill terminates
the process group. Inspect the resulting state afterward.

**Stop run** cancels agent work but is not a guarantee that every remote
operating-system process has stopped. A command can outlive a canceled tool
call or a disconnected client. Ordinary commands can also exit while leaving
child services alive. Check and stop those processes explicitly, or close a
disposable environment when its entire machine is no longer needed.

Process operations are not automatically safe to retry. After an uncertain
connection failure, inspect the machine or existing handle before asking the
agent to execute the same side-effecting command again.

## Submit jobs with dependencies

In the profile or idle session setup, enable **Environments → Durable jobs**.
The environment must advertise job support too. The agent receives:

| Tool | Use |
| --- | --- |
| `job_run` | Run one bounded job and return its terminal result. |
| `job_submit` | Submit a group and receive a promise for each job. |
| `job_read` | Inspect a job's status, bounded output, and available artifact metadata. |

Use `job_run` for a check the agent should wait for directly. It has a default
30-minute timeout and a maximum of 60 minutes. Use `job_submit` when the agent
needs dependencies or wants to do other work before collecting results.

For the release check, ask it to submit a check job followed by a report job
that runs only if the check succeeds. These model-facing `job_submit`
arguments show that relationship. Replace both directory placeholders with
the directory prepared above:

```json
{
  "jobs": [
    {
      "job_id": "release-check-1",
      "name": "check",
      "argv": ["sh", "check-release.sh"],
      "cwd": "<absolute task directory>",
      "timeout_ms": 60000
    },
    {
      "job_id": "release-report-1",
      "name": "report",
      "argv": ["sh", "-c", "printf 'Release-note structure passed\\n' > check-result.txt"],
      "cwd": "<absolute task directory>",
      "depends_on": [{"name": "check"}],
      "timeout_ms": 60000
    }
  ]
}
```

The default dependency policy is `allSucceeded`. If the check fails or is
canceled, the report becomes `dependencyFailed`. Use `allTerminal` for a
follow-up that should run after any terminal outcome, such as cleanup.
Independent jobs can run concurrently. A shared `queue_key` serializes jobs
in acceptance order within the environment's job namespace, including jobs
from separate submissions.

Names identify dependencies within a submission. Job IDs identify executions;
use fresh IDs for a deliberately new run of this example. Jobs accept an
argument array, working directory, environment variables, timeout, and
one-shot stdin, but have no interactive input channel or PTY.

## Wait, detach, or cancel

The agent passes returned promise IDs to the generic `await` tool. An await
timeout stops waiting for that interval; it does not cancel the job. The
generic `cancel` tool requests cancellation of the job associated with a
promise. Use `job_read` to inspect the job's terminal status before assuming
the process has stopped; the promise can settle before that happens.

Promises from `job_submit` initially belong to the current run. Pending jobs
are canceled when that run ends. If the task should continue across later
session turns, tell the agent to **detach the returned promises before
finishing the run**. Detached promises belong to the session and are canceled
when that session closes.

For this first exercise, ask the agent to await both jobs before answering,
check their statuses, and read `check-result.txt` from the environment. The
same promise controls are explained in
[Sub-agents and federation](../using-lightspeed/subagents-and-federation.md#join-a-result-or-use-a-promise).

Job handles include their originating environment ID. Unlike ordinary process
continuation, reading a job by its handle still targets that machine after the
session selects another active environment.

## Know what survives a restart

Workflow supervision lets a session continue following an admitted job across
runtime or client reconnections. The job record and output belong to the
environment's job service; there is no central Lightspeed job registry that
copies the machine's execution state.

The included daemon persists job records and output under its state directory.
When that daemon restarts, previously unfinished records become `interrupted`.
The jobs are not resumed transparently. Completed records remain readable
when the state directory survives; destroying the machine can destroy those
records and its output files.

Inspect terminal statuses such as `succeeded`, `failed`, `cancelled`,
`timedOut`, `dependencyFailed`, `interrupted`, or `lost`. A missing record is
not evidence of success. Jobs also attempt to clean up descendants left after
their main process exits; an orphaned-descendant result needs investigation.

The daemon currently returns no discovered artifacts in the optional artifact
list. A job writing a file does not automatically export it. Read and retain
output files explicitly before [closing the environment](power-and-cleanup.md).

## Start jobs through the API

Applications can call `environments/jobs/create`, `environments/jobs/read`,
and `environments/jobs/cancel`. These public RPCs use camelCase fields, while
the model tool examples above use their own schemas. There is no public
session process-start RPC; `process/start` belongs to the separate environment
protocol.

For example, this is a complete `environments/jobs/create` params object:

```json
{
  "environmentId": "<environment-id>",
  "requestId": "release-smoke-1",
  "jobs": [
    {
      "name": "smoke",
      "argv": ["sh", "-c", "printf 'Environment job ran\\n'"],
      "timeoutMs": 60000
    }
  ]
}
```

Retain the request ID and identical inputs when retrying the same admission.
If omitted, job IDs are derived from the environment, request, and job index.
Reusing an existing job ID with different job inputs conflicts. Fresh intended
work should use a new request ID and new explicit job IDs when supplied.

The response supplies job handles, not session promises. These standalone jobs
belong to the environment. Read using the returned handles, inspect each
entry's status and error, and advance output reads with `outputNextSeq` as
the next `afterSeq`. Output chunks contain base64 data. Cancellation can target
the individual job or its dependents; it may initially report
`cancelRequested`, so continue reading until terminal.

A standalone call can return `environment_not_ready` during provisioning or
wake-up. Wait and retry with the same creation identity. Exact types and
methods are in the [API reference](../../../crates/api/contract/api-reference.md).

## If execution does not finish as expected

| Symptom | What to check |
| --- | --- |
| The tool returned but the command is still running | A yield returns control without killing the process. Continue its handle. |
| Input cannot be delivered | Check whether the process started with a PTY and whether the model's process surface supports interactive input. |
| Output is incomplete | Check truncation indicators and read a log file if complete output was saved. |
| A background job stops when the agent answers | Await it within the run or detach its promise deliberately. |
| A dependent job never starts | Inspect prerequisite outcomes, dependency policy, and any shared queue key. |
| A job becomes interrupted after a machine restart | The daemon preserved its record but could not resume the old execution. Inspect effects before starting fresh work. |
| A credential variable is rejected | A bound name cannot also be supplied in explicit `env`. See [Environment credentials](credentials.md). |
