# harness

`harness` is the deterministic core of the Lightspeed-native agent harness. It defines the
session-scoped command, event, state, context, tooling, admission, projection,
planning, workflow helpers, and the substrate-neutral CoreAgent drive machine
used by local and Temporal substrates.

The crate intentionally does not execute provider calls, runtime tools, shell
commands, Temporal workflows, or production persistence. Those belong to local
runtimes, workflow activities, adapter crates, and storage packages.

Current architecture:

- [Agent loop and durability](../../docs/documentation/how-it-works/agent-loop-and-durability.md)
- [Context and storage](../../docs/documentation/how-it-works/context-and-storage.md)
- [Architecture overview](../../docs/documentation/how-it-works/architecture.md)

Local verification:

```bash
cargo check -p harness
cargo test -p harness
```
