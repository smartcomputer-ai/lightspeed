# P116: Transient Provider Errors Should Not Fail A Long Agentic Run

**Status**

- Implemented 2026-08-05, generation and compaction together. The retry
  disposition survives every layer it was previously erased at:
  `LlmApiError::{retryable, retry_after}` (with `ProviderFailureKind::Other`
  now terminal-by-default) → `LlmAdapterError::Provider` retains the typed
  client error → `LlmRuntime` maps to the new
  `CoreAgentIoError::{Failed, Retryable}` → the `llm_generate` /
  `context_compact` activities return the typed retryable
  `llm_provider_transient` application failure (details payload with
  attempt + clamped suggested delay honored via `next_retry_delay`) →
  bounded `llm_activity_options()` (150s start-to-close, 15min
  schedule-to-close, 2s→60s backoff ×2, 15 attempts) → the session workflow
  recognizes the typed failure in the cause chain on exhaustion and converts
  it into a terminal failed generation/compaction result with an exhaustion
  `failure_ref` (fallback: the new well-known
  `llm_runtime_boundary_failure` engine blob). Terminal provider errors keep
  the existing one-attempt failed-result path; cancellation propagates
  unchanged; the in-process test-support runner treats `Retryable` as
  terminal.
- Live-validated 2026-08-05 (temporal_live 20/20, including a real-OpenAI
  run on the new options):
  `temporal_live_transient_llm_failures_retry_within_the_turn` (two scripted
  transient failures then success → one completed run, one assistant
  message, no transcript trace) and
  `temporal_live_exhausted_llm_retries_fail_the_run_not_the_session`
  (exactly the 15-attempt budget of transient failures → run fails; the
  next run on the same session succeeds). The scripted 50ms suggested delay
  drives the full budget in seconds via `next_retry_delay`, without
  touching production constants.
- Discovered in production on 2026-08-04 during the first end-to-end Foundry
  job (a Platform-managed session developing a pack on the `hz02-devbox`
  environment).
- Separate from P114's delivery scope: P114 decomposes and bounds *tool*
  activities. This work applies the same bounded-activity and
  workflow-boundary failure principles to provider calls, with an independent
  LLM retry policy.

## Incident

Session `foundry:v1:hello`, run 1, turn 19 failed with:

```text
core agent LLM generation failed
error=core agent I/O failed: provider call failed: openai:responses
provider HTTP error 503: Our servers are currently overloaded.
```

OpenAI's status page showed an active load incident with mitigation in
progress. One transient upstream 503 discarded an 18-turn agentic run
mid-flight. The session itself survived (event-sourced state was intact), and
a manual `session/runs/start` resumed the work, but the run and its
controller-owned job failed. Recovery required an operator to notice,
diagnose, and resubmit it.

## Problem

Capacity, rate-limit, and transport failures are expected to clear within
seconds or minutes. Treating one such failure as a terminal generation result
is disproportionately expensive in a long agentic run: retrying the pending
turn is cheap, while failing the run can discard an hour of otherwise durable
progress and force a human resubmission loop.

The current path already begins with most of the necessary provider facts but
erases them before the Temporal boundary:

```text
llm-clients ProviderHttpError { retryable, retry_after, ... }
  -> llm-runtime LlmAdapterError::Provider { message }
  -> engine CoreAgentIoError::Failed { message }
  -> llm_generate returns Ok(LlmGenerationStatus::Failed)
  -> Temporal records a successful activity completion
  -> the drive fails the turn and run
```

Consequently, adding a retry policy to the activity options alone would do
nothing: Temporal retries failed activities, not successful activities whose
payload describes a failed generation. `llm_generate` also still uses the
generic `activity_options()`, unlike the bounded, purpose-specific tool
options introduced by P114.

## Decision 1: Preserve A Retry Disposition, Not Provider Taxonomy

Provider-specific classification belongs in `llm-clients`. It already has
`ProviderFailureKind` and derives `retryable` plus an optional `retry_after`.
`llm-runtime` and the generic I/O boundary should preserve the resulting
behavioral decision instead of flattening it to a string or reclassifying
OpenAI/Anthropic details.

The cross-layer contract needs only:

- the diagnostic message;
- whether the exact operation may be retried; and
- an optional provider-suggested delay.

Extend the existing error with one behavior-specific variant while preserving
the current terminal variant and its call sites:

```rust
pub enum CoreAgentIoError {
    Failed {
        message: String,
    },
    Retryable {
        message: String,
        retry_after: Option<Duration>,
    },
}
```

This avoids a `retryable: bool` that could contradict `retry_after`, and avoids
a separate `RetryDisposition` that would duplicate the same distinction.
Within `LlmAdapterError`, retain the typed `llm_clients::LlmApiError` as the
provider error source instead of copying only its message. `LlmRuntime` then
owns the single mapping from client classification to
`CoreAgentIoError::{Failed, Retryable}`.

Do not introduce a second generic failure-category enum unless a future engine
or workflow decision actually needs categories. In particular,
`ProviderFailureKind` stays inside `llm-clients`; it is not promoted into
engine or Temporal DTOs. Terminal remains the safe default when an error does
not carry explicit transient evidence.

Initial classification policy:

| Retry the same provider call | Fail the generation immediately |
|---|---|
| HTTP 408 | authentication or access denied |
| temporary HTTP 429 rate limiting | invalid request or context length exceeded |
| HTTP 500–599 | quota exhausted, including `insufficient_quota` on 429 |
| retryable connection, DNS, and request-timeout failures | unsupported model/API operation or model not found |
| retryable stream interruption before a result is committed | content/policy rejection |
| | malformed local configuration, missing adapter, or secret-resolution failure |

Unknown errors must not inherit a long retry budget merely because their kind
is `Other`; change `ProviderFailureKind::Other` from retryable-by-default unless
transport or status evidence explicitly says otherwise. Decode errors are also
terminal initially. A small decode-specific retry allowance can be considered
later if production evidence shows malformed transient responses.

A provider response that completed normally but reports a semantic failed or
cancelled status remains an ordinary generation result. The activity-failure
path is for failures that prevented Lightspeed from obtaining such a result.

## Decision 2: Retry Only The Pending Provider Activity

Retry the exact `llm_generate` activity for the active turn. Never retry the
entire run: doing so could repeat completed tool calls and earlier provider
turns.

Every attempt must carry identical logical input, including:

- session, run, and turn identity;
- provider and model selection;
- request fingerprint and revisions; and
- the same provider-native request body derived from those inputs. Live
  runtime-owned transport credentials are still resolved immediately before
  each provider attempt.

The run and turn remain active while Temporal durably backs off. Attempts do
not create new turns or semantic CoreAgent events. If an attempt eventually
succeeds, the transcript contains one successful generation and no trace of
the transient failures.

Add dedicated `llm_generate_activity_options()` rather than reusing generic or
tool activity options. The policy is runtime/deployment-owned and bounded in
both attempts and wall-clock time. Proposed initial defaults, to be validated
against the configured provider request ceiling, are:

- start-to-close: the provider request ceiling plus small completion grace
  (the configured transport request timeout is 120 seconds, so 150 seconds);
- schedule-to-close: approximately 15 minutes;
- initial retry interval: 2 seconds;
- backoff coefficient: 2;
- maximum interval: 60 seconds; and
- maximum attempts: 15.

`schedule-to-close` is the authoritative total budget, so long request
timeouts naturally permit fewer attempts than immediate 503 responses. No LLM
activity receives Temporal's unlimited default retry policy.

For a retryable provider/runtime error, `llm_generate` returns a retryable,
typed `ApplicationFailure` such as `llm_provider_transient`. Include bounded
failure details needed at the workflow boundary: the message, activity
attempt, and suggested delay. The current Rust Temporal SDK supports
`ApplicationFailure::next_retry_delay`, so honor `Retry-After` in the initial
implementation, clamped to the policy's maximum retry interval so a large
provider-suggested delay cannot schedule past the bounded total budget. Do
not sleep inside the activity; Temporal must own the delay so it is durable
across worker restarts, cancellable, and observable.

The honored `next_retry_delay` also makes exhaustion testable: a scripted
fake provider suggesting a millisecond-scale delay drives the full attempt
budget in seconds without touching production retry constants.

Use a compact versioned detail payload for that application failure:

```rust
#[derive(Serialize, Deserialize)]
pub struct LlmTransientFailureDetails {
    pub version: u32,
    pub message: String,
    pub attempt: u32,
    pub retry_after_ms: Option<u64>,
}
```

The workflow already retains the original request, so this payload does not
duplicate session, run, turn, provider, model, or request fingerprint. Those
facts can be joined back in when constructing the terminal failure blob and
emitting logs.

Terminal provider/runtime errors continue to become failed generation results
without an activity retry. Unexpected activity setup, serialization, storage,
or workflow-invariant failures must not be mislabeled as provider outages.

## Decision 3: Exhaustion Fails The Run, Not The Session Workflow

After Temporal exhausts a retryable provider activity, `call_llm_generate`
receives an activity failure. It must recognize the typed provider failure and
convert it at the workflow boundary into the same terminal
`LlmGenerationResult::Failed` shape consumed by
`CoreAgentDrive::resume_generation`. Recognition must walk the failure's
cause chain, not only the top-level failure: when schedule-to-close expires
during a backoff, Temporal reports a timeout failure whose cause is the last
typed application failure.

Construct a bounded failure message stating that provider retries were
exhausted, including the final attempt count, elapsed time when available, and
last provider error. Persist it through the existing bounded `put_blob`
activity and attach the resulting `failure_ref`; if even that materialization
fails, fall back to a well-known engine blob (the LLM analog of P114's
`tool_runtime_boundary_failure`), so the failure path itself can never retry
unbounded. This mirrors P114's conversion of an exhausted per-call tool
activity into a terminal call result. The engine sees only the final outcome
and remains substrate-neutral.

Only the recognized provider-transient activity failure takes this path.
Cancellation during a backoff propagates as cancellation, exactly as P114's
tool boundary preserves it. Failure to materialize the final blob, corrupted
activity data, and engine or workflow invariants still propagate as workflow
failures so operational bugs remain visible.

The in-process `test-support` runner has no Temporal substrate; it treats a
`Retryable` I/O error the same as a terminal failure rather than growing its
own retry loop.

The required session semantics are:

```text
503, 503, success       -> run stays active, then one successful generation
503 budget exhausted   -> run fails with an exhaustion failure_ref
401 / invalid request  -> one attempt, then run fails immediately
all provider outcomes  -> session workflow remains alive for a later run
```

Controllers such as Channels and foundry should not implement their own
provider retry or resubmit-once loops. Retrying below the run boundary avoids
duplicate controller jobs and preserves one clear owner for retry policy.

## Context Compaction

Context compaction uses the same provider clients and currently loses the same
retry metadata. Give `context_compact` equivalent transient classification,
dedicated bounded activity options, and workflow-boundary failure
materialization. Its final exhausted outcome remains a terminal
`ContextCompactionResult::Failed`; intermediate attempts are runtime facts,
not context events.

Compaction ships in the same pass as generation: it is the same
`executor.rs` mapping, the same options function, and the same boundary
helper, so deferring it would only duplicate context.

## Relationship To P114

Reuse the principles and, where useful, small boundary helpers established by
P114:

- dedicated activity options with explicit start-to-close,
  schedule-to-close, and finite attempts;
- Temporal-owned durable backoff rather than in-activity sleeping;
- conversion of an exhausted expected activity failure into a terminal domain
  result at the workflow boundary; and
- activity logs/metrics for attempts rather than semantic session-log noise.

Do not merge the policies. P114 grants retries only to tool bindings admitted
as redispatch-safe because tools may mutate external state. LLM generation is
one already-isolated activity per turn and is safe for transcript state, but
may duplicate provider work or billing. It therefore needs its own retry
budget and observability. P114's per-call tool refactor neither blocks nor
implements this work.

## At-Least-Once Provider Boundary

If a provider accepts a request but the connection fails before Lightspeed
receives the response, a retry can duplicate provider work or billing. Durable
transcript state remains correct because no generation result was committed,
but the external operation is inherently at-least-once.

Use the stable request fingerprint as an idempotency or response-recovery key
where a provider supports that capability. Otherwise document and measure the
cost boundary; failing a long run immediately is generally worse than the
small risk of duplicate generation cost.

Automatic failover to another model or provider is a separate policy. It
changes generation semantics and may be incompatible with provider-native
conversation state, so it must not occur implicitly as part of retry.

## Observability

Do not append one session event per retry. Attempts are runtime diagnostics and
would add log noise and risk leaking transient errors into model context.
Activity logs and metrics should include:

- universe, session, run, and turn;
- provider, model, and request fingerprint;
- activity attempt and elapsed retry time;
- failure disposition, HTTP status when present, and bounded message;
- provider `Retry-After` and selected next delay; and
- whether the activity will retry or has exhausted its budget.

Clients may initially continue to show a nonterminal run as “in progress.” If
product-visible “waiting for provider” state becomes important, add an
explicit workflow projection later rather than changing model context or
overloading a semantic turn event.

## Delivery Order

1. Make unknown provider failures terminal by default and add focused client
   classification tests, including temporary-rate-limit versus quota 429s.
   — Done.
2. Preserve the minimal retry disposition and optional delay through
   `llm-runtime` and the generic LLM I/O boundary. — Done.
3. Add bounded `llm_activity_options()` and return typed retryable
   activity failures for transient provider errors. — Done (one shared
   options function for generation and compaction).
4. Convert exhausted typed failures into a failure blob and terminal
   generation result at the session workflow boundary. — Done.
5. Add attempt logs/metrics and cancellation/restart coverage. — Attempt
   logging ships as a structured worker warning per transient attempt
   (operation, attempt, delay, bounded message); each attempt is otherwise
   individually visible as its own Temporal activity attempt, which P114
   already established as sufficient observability. Restart/cancellation
   semantics are Temporal-owned (durable backoff) and covered by the live
   exhaustion test crossing many attempts.
6. Apply the same provider-transient policy to context compaction. — Done in
   the same pass.

## Acceptance

- `503, 503, success` produces one successful turn and does not fail the run.
- Every attempt uses the same run, turn, request, and request fingerprint.
- Exhausted transient retries fail the run with useful attempt/exhaustion
  detail but do not fail or close the session workflow.
- Authentication, invalid-request, quota, configuration, and other terminal
  errors perform one provider attempt and fail immediately.
- Temporary 429 honors classification and `Retry-After` within the bounded
  total policy; quota 429 does not retry.
- A worker restart during backoff resumes the same activity retry sequence.
- Run/session cancellation during backoff prevents further provider attempts
  and preserves cancellation rather than converting it to failure.
- Completed tools and earlier turns are never repeated.
- Intermediate transient failures are not appended to the session log or
  included in model context.
- Context compaction receives equivalent transient retry behavior.
- Unknown internal or invariant failures are not reported as exhausted
  provider outages.

## Non-Goals

- Retrying an entire run or controller-owned job.
- Automatic provider/model failover.
- Unlimited retry or waiting indefinitely for a provider recovery.
- A generic failure-category taxonomy or separate `RetryDisposition` in
  `engine`.
- Durable per-attempt session events or exact client-visible retry progress.
