# P47: Agent Runner Core

**Status**
- Complete

Implemented so far:

- Added `crates/forge-agent/src/runner/` as the substrate-neutral runner core.
- Added `SessionRunner` in `runner/drive.rs` to replay session state from
  `SessionStore`, admit a command, append/apply committed events, run
  `PolicyPipeline`, hand created effects to an executor, and drive until
  quiescence or an iteration limit.
- Added runner protocol records: `RunnerStores`, `DriveCommand`,
  `DriveSession`, `DriveOutcome`, `RunnerQuiescence`, and pending effect
  summaries.
- Added the effect execution handoff boundary with `EffectExecutor` and
  `EffectExecution::{Receipt, Dispatched, Abandoned}`.
- Added `EffectEvent::Dispatched` and projection support so a runner can durably
  mark created effects as handed off without scheduling them twice.
- Added `SessionState::new` / `Default` as the canonical empty replay state.
- Added deterministic tests for command rejection, dispatched effects, inline
  fake LLM receipt completion, and iteration-limit quiescence.
- Made runner protocol `max_steps` optional on requests, with a conservative
  default still enforced before the drive loop runs.
- Replaced per-wave append/apply in `runner/drive.rs` with a staged proposal
  buffer that applies to working state immediately and flushes through
  `SessionStore::append` only at effect/return safety boundaries.
- Added runner coverage for the full inline tool-call path:
  LLM tool-call receipt, tool batch start, tool invocation effect, tool receipt,
  result context, batch completion, next turn, final LLM receipt, and run
  completion.
- Fixed `CoreToolPolicy` so completed tool batches suppress duplicate batch
  starts for the same tool-call turn.
- Replaced string-only `DriveOutcome` command rejections with
  `CommandRejection { kind, message }` so callers can branch on coarse rejection
  categories while retaining human-readable detail.
- Renamed the shared state-machine error from `ModelError` to `DomainError`,
  removed stale identifier/blob variants, and renamed `InvalidTransition` to
  `InvariantViolation`.
- Decided that session stream creation stays with storage/substrate runners.
  `SessionRunner` assumes the session record already exists and treats
  `SessionCommand::OpenSession` as the first domain command in that stream.
- `cargo check -p forge-agent` and `cargo test -p forge-agent` pass.

## Goal

Implement the substrate-neutral runner core that drives the P46 session state
machine from commands to committed events, policy proposals, effect intents,
effect receipts, and quiescence.

The runner core owns the deterministic domain drive algorithm. It does not own
provider APIs, host tool execution, process management, Temporal workflow
mechanics, projection rendering, or production persistence.

## Design Position

The runner core executes this loop over the committed session log:

```text
load/replay state
admit command
stage/apply admitted events
decide next proposals
stage/apply deterministic proposals
flush staged events before external effect handoff or return
execute or dispatch ready effects only after their causal facts are durable
append effect receipts or dispatch/abandonment facts
repeat until quiescent
```

`runner/drive.rs` is the algorithm boundary. `runner/protocol.rs` defines the
data contracts exposed to process, Temporal, tests, and future service
substrates. `runner/effects.rs` defines the effect handoff protocol but contains
no provider/tool implementation details.

Deterministic command and policy proposals may be buffered in memory and
appended as a causal batch for throughput. The safety rule is that no admitted
command may be acknowledged and no external effect may be executed or dispatched
until the buffered facts that justify it are durable. Effect receipts,
dispatches, cancellations, and abandonments are external boundary facts and must
be appended promptly, after which deterministic follow-up proposals may be
buffered again until the next flush boundary.

The storage contract already supports this shape:
`SessionStore::append(AppendSessionEvents { events: Vec<_>, .. })` commits a
batch and assigns contiguous session-local sequence numbers. No session-store
contract change is needed for runner batching; the work is in `runner/drive.rs`.

## Scope

- Use the existing P46 transition traits:
  `CoreAdmitCommand`, `CoreApplyEvent`, and `PolicyPipeline`.
- Use logical `SessionStore` and `BlobStore` contracts.
- Preserve event-log authority: append first, then apply returned
  `SessionEntry` values.
- Expose quiescence explicitly:
  idle, closed, waiting on effects, or iteration limit reached.
- Support inline fake effects and externally dispatched effects through the same
  `EffectExecutor` boundary.
- Batch deterministic command/policy events where safe:
  flush before external effect handoff, before returning a successful drive
  outcome, and before returning iteration-limit quiescence.

## Out Of Scope

- OpenAI/Anthropic adapters.
- Host shell/filesystem tools.
- Tokio/process runner API.
- Temporal workflow/activity implementation.
- CLI rendering.
- Production storage backends.
- Projection sinks.

## Deferred Follow-Ups

- Add command metadata/correlation propagation once command metadata is modeled
  beyond `SubmissionId`.
- Decide whether dispatched effects need additional scheduling metadata beyond
  `EffectEvent::Dispatched { effect_id }`.

## Done When

- The runner core can drive open, submit, policy proposals, LLM effect
  scheduling, inline LLM receipts, and terminal run completion.
- The effect handoff boundary is stable enough for a process runner and
  Temporal runner to share.
- No process, Temporal, provider, host-tool, or CLI implementation leaks into
  `forge-agent`.
- `cargo check -p forge-agent` and `cargo test -p forge-agent` pass.
