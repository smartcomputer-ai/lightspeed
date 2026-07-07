# P102: ACP And A2A Protocol Adapters

**Status**
- Later / exploratory.
- Written 2026-07-08 while P92/P94 are settling the native
  session-to-session communication model.
- Uses P94's planned name `SubmitMessage`; the P92 implementation currently
  calls this command `DeliverMessage`.
- References:
  - ACP Agent Run Lifecycle:
    <https://agentcommunicationprotocol.dev/core-concepts/agent-run-lifecycle>
  - ACP Distributed Sessions:
    <https://agentcommunicationprotocol.dev/core-concepts/distributed-sessions>
  - A2A Life of a Task:
    <https://a2a-protocol.org/latest/topics/life-of-a-task/>

## Goal

Expose Lightspeed sessions and runs through standards-oriented agent
protocol adapters without changing the internal execution model.

The internal model should remain:

```text
Session       = durable conversation / collaboration context
Run           = immutable unit of agent work
RequestRun    = submit work that must create or return a run
SubmitMessage = submit an inbound message; the receiver decides consume vs run
ResolvePromise = record a promise fact
ResumeAwait   = trusted workflow-to-engine command that validates and applies
                an already-visible wake condition
```

ACP and A2A adapters should translate their protocol objects into these
commands and projections. They should not become alternate engine semantics.

## Fit

Lightspeed is not primarily an interop protocol. It is a deterministic,
event-sourced execution substrate with structured concurrency, promise
resolution, cancellation, mailbox delivery, idempotent submissions, and
Temporal-backed recovery.

ACP and A2A are client/agent or agent/agent protocol surfaces. They are useful
as adapter layers, but they should enter Lightspeed through the same
admission boundaries as native clients and fleet tools.

## Protocol Comparison

| Concept | Lightspeed | ACP | A2A |
|---|---|---|---|
| Conversation scope | `Session` | `session_id` / session descriptor | `contextId` |
| Unit of work | `Run` | `Agent Run` | `Task` |
| New work | `RequestRun`, `agent_request`, `agent_spawn` | `POST /runs` | `SendMessage` returning a `Task` |
| Fire-and-forget input | `SubmitMessage` | less central | `SendMessage` within `contextId` |
| Awaiting input | parked `await { mailbox: true }` | `awaiting` state plus resume endpoint | `input-required` task state |
| Follow-up after completion | new run in same session | new run / session continuation | new task in same `contextId` |
| Terminal result | run terminal output resolves promise | completed run output | completed task artifacts/status |

ACP is closest at the single-run lifecycle layer. It defines runs that move
through created, in-progress, awaiting, completed, cancelling, cancelled, and
failed states. Its await model pauses a run until a client resumes it, which
maps naturally to Lightspeed parked runs. ACP is less precise for
multi-agent orchestration because its resume endpoint is a client-facing run
operation, while Lightspeed's `ResumeAwait` is an internal workflow command
that validates engine-visible wake state.

A2A is closer to the shape of Lightspeed's session/agent communication
system. A2A uses `contextId` to group related messages and tasks. Terminal
tasks are immutable; follow-ups and refinements create new tasks in the same
context, optionally referencing previous task ids. That maps cleanly to
Lightspeed sessions as contexts, runs as immutable tasks, and follow-ups as
additional runs in the same session.

The main difference is Lightspeed's receiver-side delivery rule:

```text
agent_send / session/messages/submit
  -> SubmitMessage

if receiver is parked with mailbox:true:
  message wakes the current run
else:
  message becomes a new message-origin run
```

P94 makes that rule engine law by logging the message buffer and validating
mailbox wakes through `ResumeAwait`. ACP and A2A do not require this exact
internal rule, but it can project cleanly to their task/run states.

## A2A Adapter Shape

Map A2A objects to Lightspeed as follows:

```text
A2A contextId          -> Lightspeed session_id
A2A taskId             -> Lightspeed run_id
A2A SendMessage        -> SubmitMessage by default
A2A SendMessage        -> RequestRun when the adapter must force task creation
A2A Task state         -> projected run status
A2A referenceTaskIds   -> context metadata pointing at prior run/artifact refs
A2A input-required     -> parked await with mailbox:true
A2A artifact           -> run output refs / produced context artifacts
```

Defaulting `SendMessage` to `SubmitMessage` preserves Lightspeed's receiver
semantics: an interactive session can consume the message as mailbox input,
while an idle session can turn it into a message-origin run. When an A2A
client requires a stable task id at request acceptance time, the adapter can
choose `RequestRun` instead.

Follow-up tasks should not reopen old Lightspeed runs. They should submit
new work in the same session, with references to the prior run's output or
artifact refs. This matches A2A task immutability and Lightspeed run
immutability.

## ACP Adapter Shape

Map ACP objects to Lightspeed as follows:

```text
ACP POST /runs              -> RequestRun
ACP GET /runs/{run_id}      -> run projection from session/read or events
ACP POST /runs/{run_id}     -> external input submitted as SubmitMessage
ACP /runs/{run_id}/cancel   -> CancelRun
ACP awaiting                -> parked await projection
ACP completed/failed/etc.   -> terminal run projection
```

ACP's client-facing resume endpoint should not call Lightspeed
`ResumeAwait` directly. External input is a new protocol fact and must enter
as `SubmitMessage` or `RequestRun`. The workflow then observes engine state,
writes any output blobs, and admits `ResumeAwait` internally.

This distinction is load-bearing:

```text
SubmitMessage = external input fact
ResumeAwait   = internal validated completion of an already-parked await
```

Allowing external protocol clients to call `ResumeAwait` would bypass the
engine's wake predicate and recreate the "trust the resume" class P94 is
designed to delete.

## Adapter Rules

- Keep all external input at normal admission boundaries:
  `RequestRun`, `SubmitMessage`, `CancelRun`, and read/projection APIs.
- Do not expose `ResumeAwait` as a public protocol method. It is a
  workflow-to-engine command.
- Preserve run/task immutability. Follow-ups create new runs, not restarted
  terminal runs.
- Preserve receiver-side delivery. Protocol adapters may choose whether a
  call requires `RequestRun` or allows `SubmitMessage`, but the receiver
  engine decides mailbox-consume vs message-origin-run for submitted messages.
- Preserve idempotency. Protocol message ids, task ids, request ids, and
  submission ids need an explicit mapping so retries do not duplicate work.
- Preserve structured cancellation. Protocol cancel requests should map to
  `CancelRun`; force-cancel remains an internal recovery/admin path.
- Project internal states conservatively. If a protocol has fewer states than
  Lightspeed, collapse states in the adapter rather than weakening the engine
  model.

## Open Design Questions

- Should A2A `SendMessage` default to `SubmitMessage`, or should the adapter
  expose a client option that forces `RequestRun` when a stable task id is
  required immediately?
- How should A2A `referenceTaskIds` be represented in Lightspeed context:
  structured metadata on submitted input, ordinary context entries, or a
  small adapter-owned index?
- Should ACP's resume endpoint target a run id but still submit session-level
  `SubmitMessage`, or should the adapter encode the target run id in message
  metadata for the receiving agent to interpret?
- How much of ACP/A2A artifact identity should be backed by Lightspeed CAS
  refs versus adapter-owned artifact ids?
- Do we need protocol-level capability discovery generated from agent
  profiles, tool manifests, or both?

## Non-Goals

- Do not make ACP or A2A the internal engine model.
- Do not expose engine reducer internals directly to protocol clients.
- Do not add a second public resume path that bypasses mailbox/promise wake
  validation.
- Do not let protocol task/message terminology reintroduce sender-side
  consume-vs-run decisions. Receiver-side delivery is a Lightspeed invariant.
