# agent-core

`agent-core` is the deterministic Forge-native agent core SDK. It defines the
session-scoped command, event, state, context, tooling, admission, projection,
planning, workflow helpers, and substrate-neutral runner-core contracts used by
later local/Temporal runners and adapters.

The crate intentionally does not execute provider calls, host tools, shell
commands, Temporal workflows, or production persistence. Those belong to later
substrate runner, adapter, and storage packages.

Current architecture direction:

- `../../spec/01-agent-idea.md`
- `../../roadmap/p53-async-agent-workflow.md`
- `../../roadmap/p54-composable-agent-kernel.md`

Local verification:

```bash
cargo check -p agent-core
cargo test -p agent-core
```
