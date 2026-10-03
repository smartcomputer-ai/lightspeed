# P144 — MCP Tool-Call Approvals

**Status**

- Implemented 2026-08-31 for the shared engine/API/client surface, the OpenAI
  Responses provider-hosted backend, and the native dispatch gate delivered
  by [P145](p145-native-mcp-execution.md).
- Supersedes the sketch in
  `later/pNNN-mcp-approval-flow.md` (production incident 2026-07-10: an OpenAI
  `mcp_approval_request` became a run's terminal output).
- Builds on P67 (provider-hosted remote MCP and opaque MCP context entries),
  P143 (the MCP server record owns `approvalDefault`), P129 (live run-control
  admissions), and P92 (parked runs). The native-execution half lands with
  [P145](p145-native-mcp-execution.md).
- One approval surface, two backends: provider-hosted MCP approvals
  (OpenAI Responses approval request/response items) and native MCP approvals
  (Lightspeed holds the tool dispatch). Clients see the same pending-approval
  model either way.
- The durable approval envelope and client surface are generic, but MCP tool
  calls are the only v1 approval subject. This leaves a narrow extension seam
  for a later binary `request_approval` workflow tool without introducing a
  general ask-user or human-task framework now.

## Problem

`approvalDefault` exists on every MCP server record, but nothing in the product
can answer an approval:

1. **Provider mode, OpenAI Responses.** `require_approval: "always"` is lowered
   into the request, and the returned `mcp_approval_request` item is preserved
   only as a `ProviderOpaque` context entry. Nothing ever constructs an
   `mcp_approval_response`, so the call can never proceed. The blast radius is
   wider than explicit `always`: `providerDefault` omits the key, and OpenAI's
   documented default is to require approval for every MCP call — so a
   `providerDefault` server on OpenAI stalls the same way.
2. **Terminal-output corruption.** When the provider reports `finish: stop`
   with no assistant message, the engine's `final_output_ref` fallback selects
   the last context entry — which in this situation is the approval-request
   JSON. The run completes "successfully" with an approval request as its
   final output, and the web transcript (which hides opaque provider entries)
   shows nothing at all.
3. **Provider mode, Anthropic.** The Anthropic MCP connector has no approval
   protocol; `RemoteMcpApprovalPolicy::Always` is a hard materialization error
   (`anthropic_messages.rs`). That refusal is correct but leaves `always`
   unusable on Anthropic sessions until native execution exists.
4. **Native mode (P145).** Once Lightspeed executes MCP calls itself, approval
   becomes entirely Lightspeed's responsibility; there is no provider to defer
   to.

Until P144, `approval: always` is unusable and the documented guidance is to
configure `never`. That inverts the safe default for exactly the servers that
most need a human gate.

## Decision

Model "an action is waiting for a human decision" as first-class session state
with one client-facing shape. MCP tool calls are the only action that creates
this state in v1, regardless of which side executes the call:

- A run with undecided approvals is **parked**, like a run waiting on an
  await. It is not complete, not failed, and holds no activity slot.
- Each pending approval is identified by a session-counter id
  (`approval_<n>`, P138 style — never the provider's opaque request id, which
  stays internal on the engine fact).
- Decisions are run-control admissions: they land on a live session the same
  way steer/cancel do, and `session/runs/cancel` records everything still
  pending as cancelled as part of cancelling the run.
- The MCP server record's `approvalDefault` remains the only policy authority
  (P143). Sessions, profiles, tool annotations, and models cannot grant,
  narrow, or bypass approval. `readOnlyHint`/`destructiveHint` may inform UI
  badges, never the decision to gate.
- `providerDefault` is removed from the policy enum (greenfield breaking,
  under P143's no-alias rules). Its meaning varied by executor — OpenAI's
  omitted key inherits OpenAI's require-approval default, Anthropic has no
  approval concept at all, and native would need a resolution rule — which
  contradicts "one configured connection, one policy". `approvalDefault`
  becomes `always | never` (default `never`, unchanged from the current API
  default), and the OpenAI lowering always writes `require_approval`
  explicitly, so Lightspeed policy never depends on a provider's current
  default.

The common approval layer records and resolves a gate; it does not decide when
one is required. That policy stays with the subject producer — the MCP server
record in v1 — so the generic envelope does not become a second authorization
framework.

### Approval lifecycle

```text
requested ──► approved ──► executed (provider continuation or native dispatch)
        └───► rejected ──► rejection result visible to the model
        └───► cancelled (run cancel; no execution or provider continuation)
```

An approval becomes terminal exactly once. A run continues only after every
pending approval in it is decided; partial decisions update state but do not
wake the run. Cancelling the run instead cancels every pending approval and the
run itself.

Approval decisions are deliberately not reusable or sticky. Each one grants or
refuses exactly one concrete provider request id or native call id. Approval
records are owned by the active run and leave materialized state with it; their
events remain the audit trail. Only the monotonic `approval_<n>` allocator is
session-scoped. A standing decision belongs in the MCP server policy (`never`),
not in an implicit cache keyed by tool, arguments, run, or session.

## Engine Model

### Typed approval facts

Stop treating approval requests as purely opaque. Keep one generic approval
envelope, with a tagged subject for what the human sees and an internal
continuation for what the engine resumes. On the generation response path,
parse the minimal deterministic facts alongside the preserved opaque entry:

```rust
pub struct ApprovalRequested {
    pub approval_id: ApprovalId,       // approval_<n>, session counter
    pub run_id: RunId,
    pub subject: ApprovalSubject,
    pub continuation: ApprovalContinuation,
}

pub enum ApprovalSubject {
    McpToolCall {
        server_id: String,
        server_label: String,
        tool_name: String,
        arguments_ref: BlobRef,
    },
}

// Durable engine state, never projected to clients.
pub enum ApprovalContinuation {
    OpenAiMcp { provider_request_id: String },
    NativeMcp { call_id: ToolCallId },
}

pub enum ApprovalDecision { Approved, Rejected }
```

This follows the existing rule: parse only the reducer facts needed for
deterministic branching; the raw provider item stays blob-backed. The decision
event records the decision, an optional bounded note, and the deciding
principal when the gateway has caller identity (P90).

`ApprovalSubject` is the public extension seam; `ApprovalContinuation` is
engine-only correlation. A later binary `request_approval` may add a workflow
subject plus `ResolvePromise { promise_id }` continuation without changing the
approval ids, decide API, projection, notifications, or clients. P144 does not
add those variants.

### Run parking

A turn that ends with undecided approvals does not complete the run and does
not plan the next turn. P144 reuses `RunStatus::Parked` and the existing P92
wake/cancellation funnel. The engine generalizes the current parked-tool-batch
holder into one run-suspension shape with a pending-approvals variant (or an
equivalent single tagged representation); it must not add a parallel parked
flag, wake loop, or cancellation path. P129 semantics extend naturally:

- steering is accepted while parked and materializes at the next turn;
- a second `session/runs/start` queues;
- `session/runs/cancel` cancels all pending approvals and drains the run through
  the ordinary cancellation funnel;
- decisions are the only non-cancellation admissions that can unpark the run.

Human rejection and run cancellation are distinct terminal approval facts. A
rejection continues the run with a refusal visible to the model; cancellation
does not manufacture a human decision or provider continuation.

### Unattended sessions: sub-agent children

A P134 child session has no approval surface: nobody watches its `RunView`,
so a parked child would hang until the parent's joined deadline kills it — a
silent stall converted into an opaque deadline failure. Children therefore
never park on approval: a session created by `SubagentExecutionWorkflow`
auto-rejects every approval request with a typed reason ("approval required,
but no approval surface reaches this session") — a deterministic rejection
result at the native gate, an explicit reject continuation in provider mode.
The refusal is model-visible, so the child reports the blocked step to its
parent instead of burning wall-clock. This is an explicit typed refusal, not
a silent auto-decision. Surfacing a child's approval request to the parent
session is a follow-up.

### Terminal-output correctness (independent fix)

Regardless of the rest of P144, fix the fallback now:

1. `final_output_ref` selection must never choose a `ProviderOpaque` context
   entry.
2. A successful generation with no assistant message either parks the run
   (undecided approvals exist) or finishes with `output_ref: None`. It must
   never surface provider bookkeeping JSON as user-visible output.
3. Regression test: an OpenAI Responses fixture containing
   `mcp_approval_request` and no `message` must not produce a completed run
   whose output ref points at the approval request.

This slice is safe to land before any approval UX exists.

## Provider Backend (OpenAI Responses)

When the response contains `mcp_approval_request` items:

1. Preserve the opaque entry exactly as today (context replay needs it).
2. Emit one `ApprovalRequested` fact per item, mapping `server_label` back to
   the session's RemoteMcp spec for `server_id`; use an `McpToolCall` subject
   and `OpenAiMcp` continuation.
3. If the same response also queued ordinary tool calls, run that tool batch
   first; park at the boundary where the next turn would otherwise be planned.
4. On full decision, plan the next turn. The adapter lowers each decision into
   the provider's continuation input:

```json
{ "type": "mcp_approval_response", "approval_request_id": "mcpr_…", "approve": true }
```

The decision travels as a typed context entry (append-only, so the provider
prefix cache holds; P137). The provider then executes approved calls and
returns ordinary `mcp_call` items; rejected calls are the provider's rejection
to explain to the model.

Anthropic provider mode is unchanged: `always` remains a hard error with a
message pointing at native execution or an OpenAI Responses model. Do not
emulate an approval loop the provider does not have.

## Native Backend (with P145)

Native execution puts the gate exactly where it belongs — before dispatch:

1. The engine plans the tool batch as usual. The gate is evaluated at the
   **dispatch boundary**: the runtime resolves the call's spec and effective
   policy, and a call that requires approval is reported back as
   needs-approval instead of executing; the engine emits an
   `ApprovalRequested` fact with an `McpToolCall` subject and `NativeMcp`
   continuation, and the call enters `awaiting_approval`. One gate location
   covers both P145 exposures — an injected call's server is knowable from its
   namespaced name, but a search-exposure `mcp_call` carries server and tool
   in arguments, which reducers do not parse. The fact always carries the
   resolved server and tool, so approval cards and transcripts never render a
   generic `mcp_call` row.
2. Approved calls dispatch through the ordinary per-call activity path.
3. Rejected calls complete immediately with a deterministic rejection result
   (`{"approved": false, "reason": …}` as a tool error), so the model sees the
   refusal and can adapt instead of stalling.
4. Calls not requiring approval in the same batch execute without waiting;
   the batch completes when every call has a result.

Effective policy in native mode is the record's `approvalDefault`, read from
the spec: `always` gates, `never` does not. With `providerDefault` removed
there is nothing to resolve, and the authored value means the same thing
under every executor.

## API

Names are illustrative; the exported contract decides the generated spelling.

```text
session/runs/approvals/decide
```

```rust
pub struct RunApprovalsDecideParams {
    pub session_id: String,
    pub run_id: String,
    pub decisions: Vec<ApprovalDecisionInput>,
}

pub struct ApprovalDecisionInput {
    pub approval_id: String,
    pub decision: ApprovalDecisionKind,   // approve | reject
    pub note: Option<String>,             // bounded, audit/display only
}

pub struct PendingApprovalView {
    pub approval_id: String,
    pub requested_at_ms: u64,
    pub subject: ApprovalSubjectView,
}

#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ApprovalSubjectView {
    McpToolCall {
        server_id: String,
        server_label: String,
        tool_name: String,
        arguments_ref: String,
        arguments_preview: String,
    },
}
```

- `RunView` gains `pendingApprovals: Vec<PendingApprovalView>`. The tagged
  subject prevents a supposedly generic view from accumulating nullable
  MCP-specific fields. Full arguments stay blob-backed and readable through
  the existing item/blob paths; backend continuation details are not
  projected.
- `runId` makes this run control like steer/cancel and prevents a stale client
  from deciding a later run's approval. Deciding an unknown, already-decided,
  cancelled, or other-run approval is a typed API error; valid decisions in
  the same request still apply (per-decision outcome in the response).
- Deciding requires exactly the authority every other run-control admission
  on the session requires (a deployment API key or universe authority, as
  with steer/cancel). There is no separate approver role or permission in v1.
- A notification/event on `session/events/read` announces newly pending
  approvals so clients do not poll `session/read`.

### Clients

Approval cards are pure frontend rendering: the web session view draws one
card per `pendingApprovals` entry (server, tool, arguments, annotation badges
as hints) with Approve/Reject buttons calling the decide API, and the run
header shows the parked state; the notification keeps the view live. No new
transport exists — any client that renders a session transcript already has
everything it needs. Arguments are model-authored text: render them as data,
never as markup or executable content.

- CLI chat: print pending approvals and accept an approve/reject input.
- Bot sessions: bot sessions are ordinary sessions, so the P141 bot page's
  session views render the same cards with no extra work; a parked run holds
  its lane and the bot event `outcome` stays pending until the run resolves.
- Channels: the chat peer sees nothing and cannot decide — a paired
  conversation's human is often not the operator, and chat access must not
  imply approval authority. The operator decides in the console. Delivering
  approval prompts into the chat itself (the personal-assistant case: "may I
  send this email?") requires an explicit per-trigger or per-pairing grant
  naming the peer as approver, and is a follow-up.

## Timeouts

v1 has no automatic approval deadline: a pending approval parks the run until
a human decides or the run is cancelled. Cancellation is the escape hatch and
must record every pending approval as cancelled on its way down, without
continuing the provider or native call. A per-server
`approvalTimeout` (auto-reject after a bound, primarily for unattended bots)
is a follow-up once real demand shows the right default; do not add a silent
auto-approve under any name.

## Implementation Slices

### Slice 1 — Terminal-output fix

- Exclude `ProviderOpaque` entries from `final_output_ref` fallback; stop
  finishes with no assistant message resolve to `output_ref: None`.
- Regression fixtures for OpenAI approval-request-only responses.

### Slice 2 — Generic approval facts and parked runs

- `ApprovalId` counter, generic request/decision/cancellation events, the
  `McpToolCall` subject and MCP continuation variants, reducer state,
  single-funnel parked-run behavior, and cancel-terminates-pending.
- Remove `providerDefault` across `mcp`, `harness`, `api`, generated clients,
  the Platform editor, CLI, and demo fixtures; the OpenAI lowering emits
  `require_approval` explicitly for both remaining values.
- OpenAI Responses parsing of `mcp_approval_request` into facts alongside the
  opaque entry.

### Slice 3 — Decide API and provider continuation

- `session/runs/approvals/decide`, projection of `pendingApprovals`,
  notifications, per-decision outcomes.
- Typed decision context entry and OpenAI `mcp_approval_response` lowering;
  live test: `always` server → park → approve → executed call; reject →
  provider rejection surfaced.
- Sub-agent child auto-reject on the provider path (explicit reject
  continuation with the typed reason).

### Slice 4 — Clients

- Web approval cards + parked state, CLI prompt flow.

### Slice 5 — Native gate (with P145)

- `awaiting_approval` call state, gated dispatch, deterministic rejection
  results, and sub-agent child auto-reject at the native gate.

## Tests

- Engine: approval facts are deterministic and replay-stable; a run with
  undecided approvals never completes; decisions are single-shot; cancel
  records pending approvals as cancelled; steering while parked lands at the
  next boundary.
- Adapter: approval-request parsing, decision lowering, prefix stability of
  the appended decision entry.
- API: decide validation (unknown/duplicate/foreign ids), projection shape,
  notification emission.
- Native (with P145): mixed batch of gated and ungated calls; reject produces
  a model-visible tool error, not a run failure.
- Policy enum: `providerDefault` is rejected everywhere it was accepted, and
  OpenAI requests always carry an explicit `require_approval` value.
- Sub-agent children: an approval-gated call in a child session produces the
  typed auto-rejection, never a parked child run.
- Regression: the 2026-07-10 incident shape end to end.

## Acceptance

1. `approval: always` is usable end to end on OpenAI Responses provider mode;
   a human can approve or reject from the web UI and CLI, and the run continues
   correctly either way. P145 applies the same surface to native execution.
2. No run ever reports an approval request (or any opaque provider entry) as
   its final output.
3. Pending approvals are visible on `RunView` with stable counter ids;
   decisions are auditable events carrying the deciding principal when
   available.
4. Cancelling a run with pending approvals records them as cancelled, performs
   no continuation, and cancels the run cleanly.
5. Anthropic provider mode still refuses `always` with an actionable message.

## Non-Goals

- Auto-approval from tool annotations, model output, or any heuristic.
- Per-session or per-profile approval overrides (P143 removed them).
- Emulating an approval protocol on providers that lack one.
- Approval deadlines/expiry policies in v1.
- Chat peers deciding approvals through channels; v1 approval authority is
  the session's run-control authority only.
- Editing or amending a call's arguments as part of approving it.
- A standing approvals inbox across sessions; v1 is per-run state.
- A model-authored `request_approval` tool or a generic ask-user interaction.
  The former can reuse this approval surface later; free-form or structured
  elicitation is a different interaction contract.

## Follow-ups

- Surfacing a child session's pending approval to its parent session.
- Channel-delivered approval prompts behind an explicit peer-as-approver
  grant on the trigger or pairing.
- Per-server `approvalTimeout` with auto-reject.
- Per-tool approval policy on the server record if All/Selected-style
  granularity proves insufficient.
- A binary `request_approval` workflow tool that creates the same approval
  envelope and resolves its existing workflow-tool promise when decided. It
  reuses approval ids, projection, notifications, decide API, audit, and UI;
  the workflow receiver remains authoritative for its payload and policy.
