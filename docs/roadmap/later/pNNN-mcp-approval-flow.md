# MCP Approval Requests And Terminal Output

**Status**

- Later / correctness follow-up.
- Discovered in production on 2026-07-10: a session with a remote Fastmail MCP
  link configured as `approval: always` received an OpenAI
  `mcp_approval_request` for `search_events`.

## Problem

`llm-runtime` preserves OpenAI `mcp_approval_request` items as opaque context
entries. When the provider reports `finish: stop` without an assistant message,
the engine's `final_output_ref` fallback currently uses the last context entry.
That makes the approval-request JSON the terminal run output and marks the run
successful. The web transcript deliberately hides opaque provider entries, so
the user sees no assistant response and no actionable approval UI.

## Required Fix

1. Model provider approval requests as an explicit pending-approval state,
   carrying the provider request id, MCP server/tool, and arguments.
2. Expose approve and reject operations through the API and UI, then submit
   the provider's required approval response and continue the run.
3. Never select opaque provider entries as a fallback terminal output. A
   successful generation with no assistant message must either remain pending
   approval or finish with `output_ref: None`; it must not report approval JSON
   as user-visible final output.
4. Add regression coverage for an OpenAI response containing
   `mcp_approval_request` and no `message`: it must not produce a completed run
   with the approval request as its output ref.

Until this is implemented, remote MCP links used through the product should
use `approval: never` (or the catalog's `providerDefault` only when it resolves
to non-interactive approval). The product currently has no approval surface.
