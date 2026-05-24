# engine

`engine` is the deterministic Forge-native agent engine. It defines the
session-scoped command, event, state, context, tooling, admission, projection,
planning, workflow helpers, and the substrate-neutral CoreAgent drive machine
used by local and Temporal substrates.

The crate intentionally does not execute provider calls, host tools, shell
commands, Temporal workflows, or production persistence. Those belong to local
runtimes, workflow activities, adapter crates, and storage packages.

Current architecture direction:

- `../../spec/01-agent-idea.md`
- `../../roadmap/p53-async-agent-workflow.md`
- `../../roadmap/p54-composable-agent-kernel.md`

Local verification:

```bash
cargo check -p engine
cargo test -p engine
```
