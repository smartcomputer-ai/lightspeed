# Appendix: Fleet One-Off Child Lifecycle

**Status:** Implemented 2026-06-24 as a small follow-up to the P84 first cut.

Fleet supports one-off child sessions for "kick off a sub-agent for this task,
let it report back, then close the child" workflows.

Model-visible `agent_spawn` lifecycle shape:

```text
lifecycle.run_immediately   bool   default true
lifecycle.close_on_terminal bool   default false
```

When `close_on_terminal = true`, the spawned child workflow receives an internal
workflow argument that tells it to admit `CloseSession` after its started run has
reached a terminal state and the child has no queued or active work. This is
runtime-enforced; the child model does not need to remember to close itself.

This is intended for one-off delegation:

1. Parent calls `agent_spawn` with `report_back` and `close_on_terminal`.
2. Child runs the task and may call `agent_send { to: parent }`.
3. Child reaches terminal.
4. Child workflow closes the child session through the normal CoreAgent
   `CloseSession` command path.

The durable session log remains readable for audit and lineage. The session is
closed, not deleted.

Notes:

- `close_on_terminal` requires `run_immediately = true`; otherwise there is no
  started run whose terminal state can trigger closure.
- Public `session/start` does not expose this lifecycle flag. It is an internal
  Fleet child-session option passed through the hosted gateway.
- Closing still uses normal CoreAgent rules: no active run, no queued run, and no
  pending context compaction.
