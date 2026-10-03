# Lightspeed TypeScript SDK

TypeScript SDK for Lightspeed, with a typed JSON-RPC client and workflow helpers.

The public API types and typed method map are generated from the committed
contract artifacts in `crates/api/contract/`. The hand-written code is limited to the
JSON-RPC transport and small workflow helpers.

For a complete application walkthrough with authentication, retry identity,
events, and results, read
[API and TypeScript](../../docs/documentation/integrating-and-extending/api-and-typescript.md).
Durable receivers and custom workers are covered in
[Workflow tools](../../docs/documentation/integrating-and-extending/workflow-tools.md).

## Install

Tagged Lightspeed releases publish `@lightspeed-ai/sdk` to npm. In-tree
consumers use the repository workspace directly.

```bash
npm install @lightspeed-ai/sdk
```

## Use

```ts
import { LightspeedClient } from "@lightspeed-ai/sdk";

const lightspeed = new LightspeedClient("http://127.0.0.1:18080/rpc");

const session = await lightspeed.call("session/start", {
  sessionId: "session_123",
  config: null,
});

const run = await lightspeed.startRun(
  session.result.session.id,
  [{ type: "text", text: "summarize this repository" }],
);

const terminal = await lightspeed.awaitRun(session.result.session.id, run.result.run.id);

console.log(terminal.state.status, terminal.cursor);
```

Raw calls return the full `AgentApiOutcome<...>` envelope, including any
notifications. JSON-RPC failures throw `LightspeedRpcError` with `code`, `message`,
`kind`, and raw `data` preserved.

`METHOD_INFO` exposes the canonical Rust-authored scope, summary, and
operational description for every method. The generated `rpc.*` helpers carry
the same text as JSDoc, while parameter and result field documentation comes
from the generated schema types.

## Regenerate

```bash
npm install
npm run check --workspace @lightspeed-ai/sdk
```

`npm run check:generated` regenerates `src/generated/*` and the packaged
schemas, and fails if the committed generated output is stale.

Workflow receivers import `@lightspeed-ai/sdk/workflow`. That subpath
contains generated emission/start-on-call types and manifest-owned constants,
plus Temporal-sandbox-safe parsing, id derivation, workflow-id, recipe, and
reply helpers. It has no Temporal package dependency.
