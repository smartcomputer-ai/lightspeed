# Reentrant Same-Session `session/runs/start` Deadlock

**Status**

- Later / correctness follow-up.
- Discovered in production on 2026-07-12 while an agent used the Configurator
  MCP to call `session/runs/start` against its own active session.
- This is an API/workflow admission problem, not primarily an MCP transport
  problem. Raising the Configurator or gateway timeout does not fix it.

## Incident

Session `session_4cdda37bd1d54a019479ab22c7d8a7b2` was running turn 3 when
the OpenAI Responses remote-MCP integration invoked the Configurator tool for
`session/runs/start`, targeting that same session.

The observed sequence was:

1. Run 3 entered `turn.generation_requested` at `14:54:42.677 UTC`.
2. During provider generation, the provider called Configurator
   `lightspeed_session_runs_start` for the same session.
3. The Configurator call eventually reported a `504` timeout. A read performed
   before run 3 finished did not show the requested run.
4. Run 3 completed at `14:57:01.031 UTC`.
5. The timed-out request was accepted as run 4 at `14:57:01.096 UTC`, only
   65 ms after run 3 completed.
6. Run 4 started and completed normally at `14:57:09.930 UTC`.

The user-visible result was therefore both a temporary self-deadlock and an
ambiguous operation outcome: the tool reported failure, but the requested work
executed later.

## Root Cause

The dependency cycle is:

```text
run 3 provider generation
  -> remote Configurator MCP tool call
    -> session/runs/start(session = run 3's own session)
      -> wait for the requested run to be accepted/started
        -> current run must make enough progress or finish
          -> provider generation is waiting for the MCP tool call
```

The hosted `session/runs/start` path durably signals
`CoreAgentCommand::RequestRun`, then
`GatewayAgentApi::wait_for_run_accepted` polls until the matching run is active
or completed. Although the API exposes `RunStatus::Queued`, this waiter does
not return a matching queued run. Admission/promotion of the requested work is
also not observed by the caller while the current provider activity is waiting
on the reentrant request.

Relevant code:

- `crates/temporal-server/src/gateway/service/mod.rs`:
  `start_run_internal` signals `RequestRun` and waits for
  `wait_for_run_accepted`.
- `crates/temporal-server/src/gateway/service/workflow.rs`:
  `wait_for_run_accepted` only returns matching active or completed runs.
- `interop/configurator-mcp/src/config.ts`: Configurator upstream calls time
  out after 60 seconds by default.
- `crates/temporal-server/src/gateway/service/mod.rs`: gateway operations time
  out after 90 seconds by default.

The Configurator timeout broke the synchronous wait seen by the provider, but
it did not retract the already-durable workflow signal. Once run 3 completed,
the signal was processed and run 4 executed. This explains the apparent
contradiction between the `504` and the later successful run.

## Required Semantics

`session/runs/start` is an acceptance/start boundary, not a terminal-output
boundary. It must not require an already-active run to finish before returning
the identity of newly accepted work.

The eventual fix should provide these properties:

1. A run request submitted while another run is active can be admitted as a
   queued run without waiting for the active run's provider or tool activity.
2. `session/runs/start` returns a `RunView` with `status: queued` as soon as the
   `RunEvent::Accepted` fact is durable. It must not wait for promotion to
   `running`.
3. Same-session calls made from a provider-hosted MCP tool cannot form a
   synchronous wait cycle with the provider generation that issued them.
4. A transport timeout or disconnect after durable submission has defined,
   retry-safe semantics. Retrying with the same submission id must discover
   the original queued/started run rather than create a duplicate.
5. Clients must not receive a definitive failure response for work that is
   still eligible to execute. If a boundary cannot distinguish whether a
   submission committed, it must expose the outcome as ambiguous and make the
   idempotent recovery path explicit.

Do not treat longer timeouts as the fix. They only extend the deadlock and make
interactive failure recovery slower.

## Implementation Questions

- Can the session workflow drain and commit admissions while an LLM/tool
  activity is outstanding, or does its drive loop currently serialize signal
  admission behind activity completion?
- Should `wait_for_run_accepted` gain a queued-run branch, or should the start
  boundary use/refactor `wait_for_run_admitted` and then project the queued
  `RunView`?
- Does `wait_for_admission_drain` unnecessarily couple client-supplied
  submission ids to unrelated pending admissions?
- Once a Temporal signal has been sent, should an HTTP disconnect cancel only
  the waiter while leaving the durable request recoverable by submission id?
  If so, document that explicitly rather than implying the operation failed.
- Is a separate fire-and-forget submission method useful for agent-to-session
  input, or is a promptly returned queued `RunView` sufficient?
- Can caller session identity be propagated for diagnostics or an early
  same-session guard? Such a guard can improve errors, but it should not replace
  correct queued-run semantics because ordinary clients can create the same
  wait pattern without MCP.

## Regression Coverage

Add a deterministic Temporal live regression with a controllable blocked
provider activity:

1. Start run 1 and hold its generation activity open.
2. Call `session/runs/start` for run 2 in the same session.
3. Assert the call returns promptly with the stable run-2 id and
   `status: queued`, without releasing run 1.
4. Assert exactly one `RunEvent::Accepted` exists for run 2.
5. Release run 1 and assert run 2 is promoted and completes normally.
6. Retry the same submission id before and after promotion; both retries must
   return run 2 and must not create run 3.
7. Abort the first HTTP/RPC waiter after durable admission, retry with the same
   submission id, and assert the committed run is discoverable without
   duplication.

Also add an end-to-end Configurator regression, using a blocked session and a
tool call to `session/runs/start`, that proves the MCP response returns the
queued run instead of timing out. This test need not invoke a live model; it can
exercise the generated Configurator tool directly against the Temporal-backed
gateway.

## Operational Guidance Until Fixed

- Do not ask an agent to synchronously call `session/runs/start` through MCP
  against its own active session.
- Starting work in a different session is safe from this particular cycle.
- Treat Configurator timeouts from mutating methods as potentially ambiguous.
  Read back using the supplied submission id or resource identity before
  retrying with a different id.

