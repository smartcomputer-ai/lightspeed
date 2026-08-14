# Lightspeed TypeScript Client

Private TypeScript client for the Lightspeed JSON-RPC gateway.

The public API types and typed method map are generated from the committed
contract artifacts in `../contract/`. The hand-written code is limited to the
JSON-RPC transport and small workflow helpers.

## Install

For private consumers, install from this repository path or a git subdirectory.
This package is not published to npm.

```bash
npm install /path/to/lightspeed/interop/ts-client
```

## Use

```ts
import { LightspeedClient } from "@lightspeed/agent-client";

const lightspeed = new LightspeedClient("http://127.0.0.1:18080/rpc");

const session = await lightspeed.call("session/start", {
  sessionId: "session_123",
  cwd: null,
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
npm run generate
npm run check
```

`npm run check:generated` regenerates `src/generated/*` and fails if the
committed generated output is stale.
