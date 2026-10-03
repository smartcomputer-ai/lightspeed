# P151 — Exec Leftover Processes, Process Handles, and Output Accounting

**Status**

- Proposed 2026-09-03, from the first Terminal-Bench 2.0 run of the Harbor
  adapter ([P149](p149-harbor-end-to-end-agent-evaluation.md)): 87 tasks,
  Lightspeed with `gpt-5.6-terra`, 58 solved. Three of the unsolved tasks
  (`pypi-server`, `configure-git-webserver`, `install-windows-3.11`) were
  lost because the service the agent started did not exist when the verifier
  ran, and four more (`fix-ocaml-gc`, `mcmc-sampling-stan`,
  `make-doom-for-mips`, `train-fasttext`) spent their budget polling builds
  with `sleep` because nothing may run between calls.
- Reviewed 2026-09-03. The first draft added a `wait_process` tool. The
  review found that the daemon, the runtime, and the Temporal activity
  deadline already carry a server-side wait for `write_stdin`, and that the
  handle contract behind it is broken in several places regardless of any
  new tool. Later revisions the same day completed the handle instead of
  adding a tool, moved the read cursor into the daemon, added a leftover
  count to the idle report, split the event-accounting fields into a
  server-only slice, and aligned the OpenAI presentation with Codex CLI at
  `728cb12fe5`.
- Final shape, 2026-09-03: a two-operation substrate (`run_process`,
  `continue_process`) that is the same for every provider, and three
  presentation surfaces on top of it. Canonical is Lightspeed's own neutral
  shape; Codex-like is the OpenAI default and copies Codex's current
  `exec_command` and `write_stdin`; Claude-Code-like is the Anthropic
  default and copies Claude Code's `Bash`, `BashOutput`, and `KillShell`.
  Fidelity is taken where it is a mapping and refused where it would change
  the substrate.
- Greenfield. Nothing here is deprecated, shimmed, or kept for older
  readers: `orphaned_descendants`, `pipe_stdin`, `WriteProcessStdin`, and
  `env.write_process_stdin` are removed, and `envd` and the server release
  together ([P152](p152-envd-release-and-distribution.md)).
- Implemented 2026-09-03, all three slices. Where the implementation had to
  decide something the design left open, or found the design's wording did
  not survive contact:
  - The data-plane protocol version is 2. The wire changes are incompatible
    in semantics even where the field shapes tolerate each other, and the
    handshake rejects a mismatched daemon rather than dual-reading.
  - `ReadProcessResponse` also carries `termination: timedOut | killed`, so
    the runtime renders `[timed out]` and `[killed]` from a fact instead of
    parsing the free-text `failure`. `ProcessStatus::Cancelled` became
    `Killed`; `StreamOutput.truncated` is gone (the daemon applies
    `max_bytes` and keeps the remainder at the cursor, so nothing is lost),
    replaced by `omitted_at`, the byte offset where the retained buffer's
    dropped middle sits.
  - Leftovers are sampled when a read is answered, not once at exit: at the
    instant the root exits a `nohup … &` child is often still the forked
    shell mid-exec, and the model should see `sleep`, not `/bin/bash`. The
    sample is `/proc` on Linux (pid, full command line) and libproc on macOS
    (pid, executable path); empty elsewhere.
  - `kill` and `interrupt` on a handle whose root already exited still reach
    the group. That is how a handle stops the service its command left
    behind without a shell round trip; the read that follows re-samples and
    the note disappears once the group is gone.
  - A read that observes a natural exit waits for the readers to reach end
    of file, bounded by the drain grace, so the last bytes a command wrote
    are in the same response as its exit code.
  - Every substrate wait is bounded: `yield_ms` and `wait_ms` absent mean
    the 30-minute ceiling, not an unbounded block, so a call always returns
    with a handle before the process activity deadline would fail it.
  - The one-shot policy is carried in the catalog binding's adapter id
    (`codex-oneshot` and so on) so the runtime applies the restricted
    defaults (Codex-like `timeout_ms` defaulting to 60 s) without a second
    catalog.
  - `outputBytes` counts the model-visible text before the projection
    budget; `truncated` is that budget cutting it. Both are stamped by the
    executing runtime on `ToolInvocationResult` and carried through the
    durable `ToolCallResult` as telemetry, like `duration_ms`.
  - Live coverage: `environment_registration_live` now drives the process
    path through the real gateway and reverse-dialed data sockets against an
    in-process `envd`: a leftover surviving its command's exit, reported by
    pid and counted by the idle report without blocking quiescence, then
    killed through the handle; the daemon cursor across two sockets; the
    empty write as a wait; PTY input; interrupt reaching the group; and the
    substrate executor's run, continue, and kill over the same transport.
    `runs_live` asserts `toolCallCompleted.outputBytes` and `truncated` on
    the parallel tool batch and `runFailed.kind` on the exhausted-retries
    run, both read from the API event log.
  - Not done: nothing kills an in-flight process when its activity is
    cancelled and the connection drops; the daemon never had such a path
    (the default 60 s `timeout_ms` was the only bound), and with
    `timeout_ms` optional an abandoned in-flight run without one now simply
    becomes a leftover of the environment. The Terminal-Bench rerun is
    tracked in the adapter repository.
- Scope is the `exec_command` process path of `lightspeed-envd`, the process
  operations and the three presentation surfaces in `crates/tools`, the
  daemon idle report, and the event fields an evaluator needs to account for
  tool output. Durable jobs are unchanged.

## Goal

A command started through the process tools may leave processes behind when
it exits normally, exactly as it would in a shell. The sandbox, not the call,
bounds their lifetime. The model is told what was left running and how to
stop it, instead of being told that the host killed it.

The handle a call returns for a still-running command is usable until the
process is gone: the model can send it input, wait on it, stop it, and read
its final output and exit code, without duplicated output and without a
`sleep` loop. Every provider gets this from one substrate; OpenAI and
Anthropic models get it under the tool names and shapes their harnesses
trained them on.

## Problem

### The sweep

`crates/environment-daemon/src/process.rs` starts every command in its own
process group (`process_group::spawn_in_own_group`). When the root process
exits, `update_exit_status` reports the group id if any member is still
alive, and every caller on the normal-exit path (`read_process`,
`write_process`, and `watch_exit`) runs `spawn_group_sweep`, which kills the
group and abandons the output readers. `ProcessOutput.orphaned_descendants`
becomes true, and `crates/tools/src/builtin/shared.rs` appends to the tool
result:

```text
[note: the command left background processes running after it exited; the
host terminated them — make the command wait for or stop its children]
```

The sweep was added with durable jobs, where a job is a supervised unit with
a defined end and stray descendants were a real problem. Applied to
`exec_command` it removes a capability every terminal task suite assumes.
The `pypi-server` transcript from the benchmark shows the whole failure in
three calls: `python -m http.server …` with a one-second timeout, reported
`Failed`; `sh -c "nohup python -m http.server … &"`, which returned the note
above; and finally the server kept alive inside a ten-minute call, which the
verifier still could not reach, because the daemon's stdout and stderr were
the call's pipes and the first access-log line after the readers were
abandoned ended it with `EPIPE`.

The note also steers the model away from the correct behavior, so the
observed pattern in long tasks is a build launched in the foreground of one
call and then polled with `sleep N; tail log` in a dozen more, each of them
a model turn.

Every harness on the Terminal-Bench leaderboard keeps leftovers alive:
Terminus 2 and OpenHands run a persistent tmux shell; Codex CLI runs
persistent sessions with a process registry; Claude Code and mini-SWE-agent
run one process per call and kill its group only on timeout, so `nohup … &`
survives. None of them sweep on a normal exit.

### The handle contract

The polling has a second cause: the handle path does not work end to end.

- The model never gets a handle. Without `yield_time_ms`, `exec_command`
  blocks until exit or `timeout_ms`, and no handle is returned. The
  description of `yield_time_ms` in `crates/tools/src/builtin/canonical.rs`
  is "Optional yield interval in milliseconds", which says nothing about a
  handle or what to do with one. So the model does not know that a long
  command can be started and waited on; it reaches for `sleep`: 17 such calls
  in `install-windows-3.11`, 15 in `train-fasttext` and `make-doom-for-mips`.
- Even with `yield_time_ms`, the model never sees the handle. A tool result
  reaches the model only through its visible text (`succeeded_tool_result`
  in `session_tools.rs` puts the JSON in the blob store for the transcript
  and gives the model the visible text alone), and `process_visible_output`
  in `crates/tools/src/builtin/shared.rs` renders stdout, stderr, and the
  note, never the handle or the running status. `write_stdin` is therefore
  unusable today: the model has nothing to pass as `handle`.
- A handle whose process has exited is dead. The only tool that touches a
  handle is `write_stdin`. The daemon's `write_process` answers `StdinClosed`
  as soon as the root has exited, and the runtime turns that into an error,
  so the final output and exit code of a process that finished between two
  calls are unreachable.
- The read cursor does not survive a tool batch. The hosted worker opens a
  fresh data-plane connection per tool activity
  (`crates/temporal-runtime/src/worker/session_tools.rs`,
  `runtime_environment_for_resource`) and closes it after the batch. The
  `RemoteProcessExecutor` built from it keeps the per-handle `next_seq` in
  memory, so a handle used in a later batch reads with `after_seq: None` and
  gets the output from sequence zero again.

Three more properties make the handle path weaker than it looks even once
those are fixed:

- The wait returns on the first output chunk. `read_process` with `wait_ms`
  returns as soon as any chunk is available, so a wait of ten minutes on a
  chatty build returns after its first line. Codex collects for the whole
  yield window unless the process exits.
- There is no way to stop a handle. The daemon has `terminate_process`, but
  the `ProcessExecutor` trait and the tool surface do not expose it. Codex
  maps a `write_stdin` of Ctrl-C (byte `0x03`) to `SIGINT` on the process
  group, on pipes as well as on a PTY.
- Retained output is unbounded. `ProcessState.chunks` keeps every chunk
  until the entry is pruned, so a running handle nobody reads grows the
  daemon's memory without limit. Codex keeps a 1 MiB head-and-tail buffer
  per session and marks the omitted middle.

The daemon already has the primitive the handle needs: `read_process` with
`wait_ms` blocks server-side until output, exit, or the deadline, and the
runtime already calls it inside `exec_command` (as `yield_time_ms`) and
`write_stdin`. The Temporal process activity deadline is already the
30-minute ceiling shared with `max_process_timeout_ms`. Nothing new crosses
the workflow or protocol boundary; the tools in front of the primitive are
incomplete.

### Canonical is Codex in disguise, and Codex has moved

The toolset picks a presentation surface per provider
(`crates/tools/src/toolset.rs`, `surface`): OpenAI targets get Canonical,
Anthropic targets get Claude-Code-like, and Codex-like exists as a variant
that delegates everything to Canonical. Canonical's process tools are named
`exec_command` and `write_stdin` after Codex, but their shapes are ours
(`argv`, `cwd`, `env`, `stdin`, `timeout_ms`, `yield_time_ms`, a required
`input`), so OpenAI models get Codex's names with none of Codex's contract,
and any other provider gets a surface that is neither neutral nor faithful.

Codex at `728cb12fe5` (`codex-rs/core/src/tools/handlers/shell_spec.rs`,
`codex-rs/core/src/unified_exec/`) is:

- `exec_command` with `cmd` (a shell string), `workdir`, `tty` (default
  false: plain pipes), `yield_time_ms` (default 10000, range 250 to 30000),
  `max_output_tokens` (default 10000), and `login`. No `timeout_ms` and no
  kill: a command still running at the yield returns a session id and keeps
  running. The `pypi-server` failure began with the model passing a
  one-second timeout, a parameter Codex never offered it.
- `write_stdin` with `session_id`, `chars` ("Defaults to empty, which polls
  without writing"), `yield_time_ms` (non-empty writes 250 ms default and
  30 s cap; empty polls 5 s to 300 s), and `max_output_tokens`.
- Stdin open only when `tty` is true. On pipes a non-empty `chars` is
  refused with `StdinClosed`, except Ctrl-C, which sends `SIGINT` to the
  group. Interactive programs are meant to run under a PTY, which our daemon
  already supports (`start_pty_process`) but the runtime never requests.
- A header the model parses: `Wall time: N seconds`, then `Process exited
  with code N` or `Process running with session ID N`, an `Original token
  count` when truncated, then `Output:` and the text.
- The session entry removed by the read that observes exit. Sessions
  survive turn interruption and are killed only at session shutdown, on an
  explicit clean-up, or when the 64-entry store prunes the least recently
  used one.
- Commands run under `setsid` as group leaders; a natural exit never kills
  the group; terminate, timeout, and prune kill it with `SIGKILL`.
- A policy-restricted one-shot variant that drops `tty` and
  `yield_time_ms`, adds `timeout_ms` (default 10 s), and omits
  `write_stdin`.

### Anthropic models have no handle path

The Claude-Code-like surface exposes one process tool, `Bash`, mapped to
`bash -lc` with `yield_time_ms` always unset
(`crates/tools/src/builtin/claude.rs`), parses `run_in_background` and
ignores it, and marks `write_stdin` unsupported so the catalog omits it. An
Anthropic model therefore has foreground commands only. Claude Code itself
expresses the handle tier as `Bash` with `run_in_background`, `BashOutput`,
and `KillShell`, and Claude models reach for those by reflex. Their names and
parameters are public; their response wording is not something we can
verify, so it stays ours.

### Jobs are not the answer for this

Durable jobs are the right shape for a build: a supervised unit with an end,
output kept as an artifact, a promise the workflow parks on so the wait costs
no model turns, and a sweep at completion that is correct for a build. They
are the wrong shape for a service, which has no end, and for anything the
model wants to interact with. And they were not available to the benchmark:
the hosted session toolset is `EnvironmentToolsetConfig::basic()` plus
`job_read`, so the model saw `exec_command`, `write_stdin`, and a `job_read`
that cannot wait (`crates/tools/src/environment/tools/jobs.rs` passes
`wait_ms: None`). Whether a profile should expose `job_submit` and
`job_run`, and whether `job_read` should wait, are separate decisions and not
needed to fix the seven tasks.

Making a yielded `exec_command` return a promise the workflow parks on was
considered and rejected: it is the jobs machinery, it pulls `exec_command`
into the workflow-tool protocol, and it does not model what handles are for,
which is interacting with a live process. Jobs stay the tier for hours-long
work; handles stay a bounded wait inside one activity.

### Evaluator accounting

The evaluator cannot account for tool output from the event log:
`toolCallCompleted` carries a status and effects but no output size or
truncation flag. `runFailed` carries a free-text message, although the engine
already classifies the failure: `RunFailure.kind` in
`crates/harness/src/core/components/run.rs` is one of `model_failure`,
`tool_failure`, `context_failure`, `limit_exceeded`, `cancelled`, `internal`,
and the API event in `crates/api/src/sessions.rs` drops it.

## Decision

1. **A normal exit does not sweep.** When the root process of a command
   exits on its own, processes left in its group keep running. Their
   lifetime is the environment's; closing or powering down the environment,
   or destroying the sandbox, ends them.
2. **Timeout, termination, and cancellation of the call in flight still
   kill the group.** `timeout_process`, `terminate_process` with `kill`, and
   cancelling an activity whose process has not been handed back keep
   today's `kill_group` plus `kill_child`. A session already returned to the
   model is not in flight and is untouched by run cancellation.
3. **Jobs keep the sweep.** `jobs.rs` is not changed; a job that leaves
   descendants behind is still swept at completion, cancellation, and
   timeout.
4. **Leftovers get somewhere to write.** After a normal exit the daemon keeps
   reading the process's stdout and stderr pipes to end of file. Output
   arriving within the existing drain grace is kept for the caller; after
   that the bytes are discarded. Nothing the leftover writes can block it or
   kill it with `EPIPE`.
5. **The model is told, not scolded.** The process response lists what was
   left running (pid and command); the note is informational and says how to
   stop it and that a power-down ends it.
6. **The substrate is two operations: `run_process` and
   `continue_process`.** Run starts a process and waits up to a yield or to
   exit. Continue takes a handle, optionally delivers input, closes stdin,
   or sends a signal (interrupt or kill), then waits for its window and
   returns new output. Every handle interaction is "optionally act, then
   wait and read", which is what the daemon does underneath, so there is one
   code path, one cursor, one renderer. Terminate is a field of continue,
   not an operation.
7. **Substrate rules are the same for every provider.** `argv`, never a
   shell string; a wait collects for its whole window; the daemon owns the
   read cursor; a finished handle stays readable for the retention period;
   unread output is a capped head-and-tail buffer; plain pipes get
   `/dev/null` stdin and interactive input requires `tty`; `timeout_ms` is
   optional, so a surface may offer a kill deadline or not; a natural exit
   never sweeps; leftovers are reported.
8. **Three surfaces, and Canonical is ours.** Canonical presents the
   substrate directly, with neutral names and descriptions written for a
   model that has seen neither harness. Codex-like copies Codex's current
   `exec_command` and `write_stdin` and is the default for OpenAI Responses
   targets. Claude-Code-like copies Claude Code's `Bash`, `BashOutput`, and
   `KillShell` and is the default for Anthropic targets. OpenAI Completions
   targets, the compatibility API most other providers speak, default to
   Canonical. A surface is a mapping table plus text; it never changes what
   the substrate does.
9. **Fidelity is taken where it is a mapping and refused where it would
   change the substrate.** Taken: names, argument names, defaults, fixed
   fields, header wording, Ctrl-C as interrupt, stdin only under a PTY.
   Refused: numeric session ids, the 64-session LRU kill, `chunk_id` and
   structured output schemas, Codex's `shell` and approval parameters,
   Codex's 30-second yield cap, Claude Code's `filter` regex.
10. **The visible text carries the state.** Running: handle and pid. Exited:
    exit code. Leftovers: pid and command. Nothing the model needs to act on
    lives only in the JSON.
11. **The idle report counts leftovers but does not treat them as busy.** A
    server waiting for requests is idle; counting it as running would keep
    environments awake indefinitely. The count lets a power policy prefer
    freezing over stopping when leftovers exist.
12. **Event accounting is a separate, server-only slice.** `runFailed`
    exposes the failure kind the engine already records; `toolCallCompleted`
    gains `outputBytes` and `truncated`. It ships with the server, not with
    `envd`.
13. **Greenfield.** Removed things are removed: no deprecated fields, no
    dual-reading of old and new names, no "older daemons omit" clauses.
    `envd` and the server release together.

## Design

### Daemon exit path

- `update_exit_status` keeps returning the live group id. The callers on the
  normal-exit path (`read_process`, `write_process`, `watch_exit`) replace
  `spawn_group_sweep` with `spawn_group_drain`: it runs the existing
  `drain_or_abort_readers` grace so bytes the root wrote just before exiting
  are kept, then lets the reader tasks continue to end of file in discard
  mode (`read_stream` stops pushing chunks once the state is marked
  drained). The pipe's own end of file is the completion signal: when the
  last leftover holding the pipe exits, the readers end on their own. A
  leftover that redirected its output to `/dev/null` closes the pipe at once
  and needs no reader. The drain never signals the group.
- The reader tasks must not be aborted by `schedule_terminal_cleanup` while
  they are draining a live pipe. Cleanup drops the process entry and the
  stored chunks; the readers hold their own `Arc` to the entry state and
  finish at end of file.
- `ProcessState` records `leftover_processes: Vec<LeftoverProcess>` sampled
  once at root exit: the live members of the group with pid and command
  line, from `/proc` on Linux, best effort and empty elsewhere. This replaces
  the `orphaned_descendants` boolean.
- `timeout_process`, `terminate_process` with `kill`, and cancellation are
  unchanged and still call `spawn_group_sweep`.
- Daemon shutdown does not kill leftover groups. `envd` exiting or being
  terminated by an orchestrator leaves the environment's processes as they
  are; an orchestrator that wants a clean sandbox destroys the sandbox.
- PTY-backed processes already outlive the call's reader because the PTY
  master stays open with the entry; they get the same leftover sampling.

### Daemon handle semantics

- `read_process` with `wait_ms` collects until the process exits, the
  deadline passes, or `max_bytes` is reached; it no longer returns on the
  first chunk. Without `wait_ms` it blocks until exit as today.
- `ProcessState` gains `delivered_seq`. `read_process` with
  `after_seq: None` reads after the last delivered chunk and advances
  `delivered_seq` to the response's `next_seq`; an explicit `after_seq` is a
  re-read and does not move the cursor. `RemoteProcessExecutor` drops
  `next_seq_by_process` and always sends `after_seq: None`.
- Unread chunks are kept in a head-and-tail buffer capped at 1 MiB per
  process; when it overflows, the middle is dropped and the next read
  carries `omitted_bytes`.
- `write_process` with no chunk and `close_stdin: false` is a wait: it
  returns `Accepted` whether or not the process has exited, and the caller's
  `read_process` follows. A non-empty chunk after exit, or on a process whose
  stdin is not open, answers `StdinClosed`. `UnknownProcess` is answered
  only after `TERMINAL_PROCESS_RETENTION` has pruned the entry, which starts
  at the first read that observed the exit, so a finished process's final
  output stays readable for that long after the model first saw it exit.
- `start_process` pipes stdin only for a one-shot `stdin` payload (written
  and closed at start) or for a PTY, whose master is the input. Plain-pipe
  processes get `/dev/null`. `pipe_stdin` is removed from the request.
- `terminate_process` gains `signal: interrupt | kill`. Interrupt sends
  `SIGINT` to the group and changes no state; the following read observes
  whatever the process did. Kill is today's path.
- `ReadProcessResponse` gains `pid`, the root's OS pid, which is also its
  process group id because the daemon spawns each command as a group
  leader.

### Idle report

- `ProcessManager` keeps the group ids of leftovers it stopped sweeping and
  drops each when `group_alive` is false. `IdleResponse` gains
  `leftover_process_groups: u32`. `is_quiescent` is unchanged: leftovers are
  not running work. The power reconciler may use the count to choose freeze
  over a stateful stop; that policy is not part of this item.

### Protocol and runtime

- `ReadProcessResponse` in `crates/environment-protocol/src/data/process.rs`
  replaces `orphaned_descendants: bool` with
  `leftover_processes: Vec<LeftoverProcess { pid, command }>` and gains
  `pid: Option<u32>` and `omitted_bytes: u64`. `StartProcessParams` loses
  `pipe_stdin`. `TerminateProcessParams` gains `signal`.
- `ProcessExecutor` in `crates/tools` becomes two methods, `run_process`
  and `continue_process`. `ProcessRequest` gains `tty`. A new
  `ContinueProcessRequest` carries `handle`, `input`, `close_stdin`,
  `signal`, `wait_ms`, `max_output_bytes`; the remote executor issues
  `process/write` or `process/terminate` as needed and then `process/read`
  with `wait_ms`; the inline runtime does the same against local processes.
  `WriteProcessStdinRequest` is removed.
- `ProcessOutput` carries `status`, `handle`, `pid`, `exit_code`, `stdout`,
  `stderr`, `omitted_bytes`, and `leftover_processes`. The projections in
  `environment_protocol/remote.rs`, `runtime/inline.rs`, and
  `session_tools.rs` follow.

### Substrate operations

`BuiltinToolOperation` and `EnvironmentToolsetConfig` change from
`RunProcess` and `WriteProcessStdin` to two operations, both in `basic()`:

| Operation | Logical id | Arguments | Result |
|---|---|---|---|
| `RunProcess` | `env.run_process` | `argv`, `cwd`, `env`, `stdin`, `tty`, `yield_ms`, `timeout_ms`, `max_output_bytes` | `ProcessOutput` |
| `ContinueProcess` | `env.continue_process` | `handle`, `input`, `close_stdin`, `signal`, `wait_ms`, `max_output_bytes` | `ProcessOutput` |

`yield_ms` and `wait_ms` are capped by `max_process_timeout_ms`, so a wait
is bounded by the process activity deadline. `timeout_ms` is optional; when
absent, a running process is never killed by the call. `crates/eval` and
any other reference to `env.write_process_stdin` follow the rename.

`process_visible_output` takes the surface and renders the state the model
needs to act on, not only the bytes. The leftover note is appended on every
surface and is absent when nothing was left:

```text
[note: 1 process is still running after the command exited: pid 91
`python -m http.server 8080`. It keeps running until you stop it or the
environment is closed or powered down.]
```

### Surfaces

`BuiltinToolPresentation::ProviderDefault` maps OpenAI Responses to
Codex-like, Anthropic Messages to Claude-Code-like, and OpenAI Completions
to Canonical; `presentation` overrides it as today. Filesystem tools are
unchanged on every surface. The process tools:

| | Canonical | Codex-like (OpenAI default) | Claude-Code-like (Anthropic default) |
|---|---|---|---|
| run | `run_process`: `argv`, `cwd`, `tty`, `yield_ms`, `timeout_ms` | `exec_command`: `cmd`, `workdir`, `tty`, `yield_time_ms` default 10 s, `login`; no timeout | `Bash`: `command`, `timeout` as kill deadline, `run_in_background` as zero yield |
| continue | `continue_process`: `handle`, `input`, `signal`, `wait_ms` | `write_stdin`: `session_id`, `chars` (empty polls, Ctrl-C interrupts) | `BashOutput`: `bash_id`, `timeout`; `KillShell`: `shell_id` |
| header | ours, plain | Codex's `Process running with session ID` lines | Claude Code's background wording |

**Canonical.** The substrate, presented directly.

- `run_process`: `argv` (required), `cwd`, `env`, `stdin`, `tty` (default
  false), `yield_ms`, `timeout_ms`, `max_output_bytes`. Description: "Run a
  command. Waits until it exits, or until `yield_ms` if set, and returns its
  output. If it is still running you get a handle for `continue_process`.
  With `timeout_ms` the command is killed at that deadline; without it a
  running command keeps running until stopped or the environment closes.
  Interactive programs need `tty: true`."
- `continue_process`: `handle` (required), `input`, `close_stdin`, `signal`
  (`interrupt` or `kill`), `wait_ms`, `max_output_bytes`. Description:
  "Continue with a running handle: optionally send input or a signal, then
  wait up to `wait_ms` and return the output produced since the last call.
  With nothing but the handle it only waits. Once the process has exited it
  returns the remaining output and the exit code."
- Visible text: the output, then one line: `[running: handle proc-…, pid
  91]` or `[exited with code N]` or `[killed]` / `[timed out]`, then the
  leftover note. When the buffer dropped output, `[omitted N bytes]` sits
  where the middle was.

**Codex-like.** Shapes are Codex's at `728cb12fe5`; text is Codex's where
it fits, plus one sentence on leftovers.

- `exec_command`: `cmd` (required; shell string), `workdir`, `tty` (default
  false; "True allocates a PTY for the command; false or omitted uses plain
  pipes."), `yield_time_ms` ("Wait before yielding output. Defaults to
  10000 ms; effective range is 250-1800000 ms."), `max_output_tokens`
  ("Output token budget. Defaults to 10000 tokens."), `login` (default
  true). Description: "Runs a command in a shell (a PTY when `tty` is
  true), returning output or a session ID for ongoing interaction. A command
  may leave services running for later calls; they keep running until
  stopped or the environment closes." Mapping: `cmd` to
  `["bash", "-lc", cmd]` (`-c` when `login` is false), `workdir` to `cwd`,
  `max_output_tokens` to `max_output_bytes` at four bytes per token, no
  `timeout_ms`.
- `write_stdin`: `session_id` (required; the handle string), `chars`
  ("Bytes to write to stdin. Defaults to empty, which polls without
  writing."), `yield_time_ms` ("Wait before yielding output. Non-empty
  writes default to 250 ms and cap at 30000 ms; empty polls default to
  60000 ms and cap at 1800000 ms."), `max_output_tokens`. Description:
  "Writes characters to an existing unified exec session and returns recent
  output." Mapping: empty `chars` to a bare wait; `chars` equal to Ctrl-C
  (`0x03`) to `signal: interrupt`; any other `chars` to `input`, which on a
  session without a PTY returns the `StdinClosed` error with the hint to
  start the command with `tty: true`.
- Visible text is Codex's header, then the output:

  ```text
  Wall time: 10.0012 seconds
  Process running with session ID proc-…, pid 91
  Output:
  <stdout and stderr>
  ```

  ```text
  Wall time: 3.2 seconds
  Process exited with code 0
  Output:
  <stdout and stderr>
  [note: …leftovers…]
  ```

  When the buffer dropped output, `Original byte count: N` precedes
  `Output:` and the omission marker sits where the middle was. The pid on
  the running line is our addition so `kill -- -91` works from a shell.

**Claude-Code-like.** Names and parameters are Claude Code's; wording is
ours except the background start line.

- `Bash`: `command`, `timeout`, `description`, `run_in_background`.
  `run_in_background: true` maps to `yield_ms: 0` and no `timeout_ms`; the
  call returns at once with the handle. Otherwise `timeout` (default
  `default_process_timeout_ms`) is a kill deadline as in Claude Code.
  Visible text for a background start: "Command running in background with
  ID: proc-…. Use BashOutput to read its output and KillShell to stop it."
  `dangerouslyDisableSandbox` stays parsed and ignored.
- `BashOutput`: `bash_id` (required), `timeout` (milliseconds, default
  `default_process_timeout_ms`, capped by `max_process_timeout_ms`),
  `filter` (parsed, not applied). Description: "Wait up to `timeout` for a
  background command to finish and return the output produced since the
  last call. Returns at once if it has already exited, with its exit code."
  Maps to `continue_process` with only `wait_ms`. Visible text ends with
  `[exited with code N]` or `[still running]`.
- `KillShell`: `shell_id`. Maps to `continue_process` with `signal: kill`
  and a short `wait_ms` equal to the drain grace. Returns the output so far
  and `[killed]`.
- No stdin input and no `tty` on this surface. Claude Code has neither and
  Claude models have no trained pattern for them; heredocs and `expect`
  cover the rare need. Adding them later is a schema change on this surface
  only.

**One-shot policy.** `EnvironmentToolsetConfig` with `continue_process:
false` renders the restricted shape on every surface from the same knob:
Canonical `run_process` without `yield_ms`; Codex-like `exec_command`
without `tty` and `yield_time_ms`, with `timeout_ms` (default
`default_process_timeout_ms`) and the description "Runs a command to
completion and returns its output. The process is terminated on timeout or
cancellation and cannot be resumed."; Claude-Code-like `Bash` without
`run_in_background` and without `BashOutput` or `KillShell`.

**Later surfaces.** The same substrate can host a persistent-shell
presentation, such as Anthropic's own `bash` tool with `restart` or a
Terminus-style shell, by holding one PTY `bash` handle per session and
sending each command through `continue_process`, with `restart` as kill plus
run. Not proposed here; noted because it shows nothing in the substrate
needs to change for it.

### Event accounting (server only)

- `runFailed` in `crates/api/src/sessions.rs` gains `kind`, the existing
  `RunFailureKind` serialized as today (`model_failure`, `tool_failure`,
  `context_failure`, `limit_exceeded`, `cancelled`, `internal`). No reducer
  change; the value is already on `RunFailure`.
- `toolCallCompleted` gains `outputBytes` (bytes the tool produced before
  projection) and `truncated` (true when the projection cut it).
- The Harbor adapter's per-trial measures (model calls, tool calls, output
  truncations, failure class) are derived from these; today truncations and
  failure class are blank.
- Regenerate the API contract and TypeScript consumers.

### Limits

Leftovers and live sessions count against nothing beyond the retained
output cap: there is no process registry, no per-environment session cap,
no reattach API. Codex prunes beyond 64 sessions by killing the least
recently used; we do not, because the environment's own resources bound it
and a killed session is a worse surprise than a slow one. If a cap is ever
wanted, it is an environment policy, not a process-tool behavior.

## Acceptance

Daemon:

- `sh -c 'nohup sleep 1000 >/dev/null 2>&1 &'` returns immediately with the
  leftover reported; `sleep` is alive after the call completes and still
  alive after the process entry is pruned.
- A leftover that writes to stdout after the root exited (`sh -c 'while
  true; do echo x; sleep 1; done &'`) is alive ten seconds later and the
  daemon's stored output for the entry does not grow after the drain grace.
- A running process nobody reads that prints 10 MiB retains at most 1 MiB;
  the next read carries the omitted byte count and both the head and the
  tail of the output.
- The same command under `timeout_ms` that expires, or terminated with
  `kill`, leaves no group member alive (existing behavior). `interrupt` on
  `sh -c 'trap "echo caught; exit 3" INT; sleep 100'` yields `caught` and
  exit code 3 on the next read.
- A durable job that leaves descendants is still swept at completion
  (existing `jobs.rs` tests pass unchanged).
- `read_process` with `wait_ms` on a process that prints every 100 ms for
  two seconds returns once, at exit, with every line; with a 500 ms wait it
  returns at 500 ms with the lines so far.
- Two consecutive `read_process` calls with `after_seq: None`, made through
  two separate connections, return disjoint output.
- `write_process` with no chunk on an exited process returns `Accepted`; the
  following read returns the remaining output and the exit code; the same
  call after `TERMINAL_PROCESS_RETENTION` reports `UnknownProcess`.
- A plain-pipe process reads end of file on stdin at once; a PTY process
  accepts a chunk and echoes it.
- `IdleResponse.leftover_process_groups` is one while the `nohup sleep`
  leftover lives and zero after it is killed; `is_quiescent` is true
  throughout.

Substrate and surfaces:

- The tool note for a leftover names the pid and command and does not say
  the host terminated anything; the note is absent when nothing was left.
- Canonical: `run_process` with `argv: ["sleep", "30"]` and `yield_ms:
  1000` returns after one second with the handle and pid in the visible
  text; `continue_process` with only the handle and `wait_ms: 60000`
  returns at exit with the exit code; `continue_process` with
  `signal: kill` on a running handle leaves no group member alive.
- Codex-like: `exec_command` with `cmd: "sleep 30"` and the default yield
  returns after ten seconds with `Process running with session ID …` and no
  kill; `write_stdin` with empty `chars` and `yield_time_ms: 60000` returns
  at exit with `Process exited with code 0`; `exec_command` with `cmd:
  "python3 -m http.server 8080"` returns a session id, the server answers a
  request made from a later `exec_command`, and `write_stdin` with Ctrl-C
  stops it; `write_stdin` with non-empty `chars` on a session started
  without `tty` returns the `StdinClosed` error naming `tty`, and the same
  on a session started with `tty: true` delivers the input.
- Claude-Code-like: `Bash` with `run_in_background` returns the handle and
  names `BashOutput` and `KillShell`; `BashOutput` waits for the whole
  window or until exit and a second call returns only new output;
  `KillShell` leaves no group member alive.
- Catalogs: an OpenAI Responses target lists `exec_command` with `cmd`,
  `workdir`, `tty`, `yield_time_ms`, `max_output_tokens`, `login` and
  `write_stdin` with `session_id`, `chars`, `yield_time_ms`,
  `max_output_tokens`, and no `timeout_ms` or `argv`; an Anthropic target
  lists `Bash`, `BashOutput`, `KillShell`; an OpenAI Completions target
  lists `run_process` and `continue_process`. With `continue_process`
  disabled each target lists its one-shot shape and no continue tool.

Server and benchmark:

- A tool result larger than the projection budget produces
  `toolCallCompleted.truncated == true` and an `outputBytes` above the
  budget; a failed run's `runFailed` carries the engine's `kind`.
- With the new daemon, the Harbor adapter's targeted rerun of
  `pypi-server`, `configure-git-webserver`, and `install-windows-3.11` lets
  the verifier reach the service the agent started. `fix-ocaml-gc`,
  `mcmc-sampling-stan`, `make-doom-for-mips`, and `train-fasttext` are rerun
  with the new surface; the adapter reports the number of `sleep` commands
  per trial, expected to drop from the dozen-plus observed to a handful.

## Non-Goals

- A process registry, `attach`/`reattach`, or named background sessions;
  run, continue, and durable jobs remain the process surface.
- A separate wait or kill tool on any surface beyond what its harness has.
- Shell strings in the substrate; `cmd` and `command` are surface mappings
  onto `argv`.
- Codex's numeric session ids, `shell` parameter, approval parameters,
  `chunk_id`, structured `output_schema`, or 30-second yield cap; Claude
  Code's `filter` regex.
- Stdin input or `tty` on the Claude-Code-like surface.
- A per-environment session cap or LRU kill.
- A persistent-shell surface; noted above as possible, not proposed.
- Changing durable-job semantics or the job sweep, exposing `job_submit` and
  `job_run` in more profiles, or making `job_read` wait. Those are their own
  decisions.
- Cleaning leftovers on daemon exit or environment close beyond what the
  sandbox teardown already does.
- Counting leftovers as running work in the idle report, or any power
  policy change; this item only reports the count.
- Changing `default_process_timeout_ms` (60 s) or `max_process_timeout_ms`
  (30 min) in `crates/tools/src/limits.rs`.

## Implementation Slices

### Slice 1 — Daemon (`envd`)

- Replace the normal-exit sweep with the drain, sample leftover processes,
  keep the sweep on timeout/terminate/cancel, and keep the job path
  untouched.
- Make the wait collect for its window, add the delivered-sequence cursor,
  the head-and-tail retained buffer, and the empty-write wait to
  `read_process` and `write_process`; add `pid` and `omitted_bytes` to the
  read response; add `signal` to `terminate_process`; remove `pipe_stdin`
  and give plain-pipe processes `/dev/null` stdin.
- Add `leftover_process_groups` to the idle report.
- Unit tests in `crates/environment-daemon` for the daemon acceptance cases
  above, including the `EPIPE` case, the wait window, the retained cap, the
  cursor across two connections, the empty write after exit, and interrupt;
  the existing `process_exit_sweeps_orphaned_descendants` test is inverted.

### Slice 2 — Protocol, runtime, substrate, and surfaces

- Protocol: `leftover_processes`, `pid`, `omitted_bytes`, `signal`; remove
  `orphaned_descendants` and `pipe_stdin`.
- Runtime: `ProcessExecutor` with `run_process` and `continue_process`,
  `tty` on `ProcessRequest`, `ContinueProcessRequest`, both executors;
  remove `next_seq_by_process` and `WriteProcessStdinRequest`.
- Substrate: `ContinueProcess` replaces `WriteProcessStdin`; `yield_ms`,
  optional `timeout_ms`, `tty` on `RunProcess`; `basic()` and logical ids;
  `crates/eval` references.
- Surfaces: Canonical `run_process` and `continue_process`; Codex-like
  `exec_command` and `write_stdin` with the `cmd`, `max_output_tokens`, and
  Ctrl-C mappings and Codex's header; Claude-Code-like `Bash` with
  `run_in_background`, `BashOutput`, `KillShell`; the provider-default
  mapping; the one-shot shape per surface; the leftover note.
- Runtime tests in `crates/tools` for the three catalogs, argument mapping,
  the bounds, and the visible text; the environment-protocol live suite
  covers the daemon round trip.

### Slice 3 — Event accounting (server only)

- Expose `kind` on `runFailed`, add `outputBytes` and `truncated` to
  `toolCallCompleted`, regenerate the contract, and derive the adapter's
  measures from them. Independent of slices 1 and 2.

