# Forge TypeScript Client

Private TypeScript client for the Forge JSON-RPC gateway.

The public API types and typed method map are generated from the committed
contract artifacts in `../../schemas/`. The hand-written code is limited to the
JSON-RPC transport and small workflow helpers.

## Install

For private consumers, install from this repository path or a git subdirectory.
This package is not published to npm.

```bash
npm install /path/to/forge/clients/typescript
```

## Use

```ts
import { ForgeClient } from "@forge/agent-client";

const forge = new ForgeClient("http://127.0.0.1:18080/rpc");

const session = await forge.call("session/start", {
  sessionId: "session_123",
  cwd: null,
  config: null,
});

const run = await forge.startRun(
  session.result.session.id,
  [{ type: "text", text: "summarize this repository" }],
);

const terminal = await forge.awaitRun(session.result.session.id, run.result.run.id);

console.log(terminal.state.status, terminal.cursor);
```

Raw calls return the full `AgentApiOutcome<...>` envelope, including any
notifications. JSON-RPC failures throw `ForgeRpcError` with `code`, `message`,
`kind`, and raw `data` preserved.

## Regenerate

```bash
npm install
npm run generate
npm run check
```

`npm run check:generated` regenerates `src/generated/*` and fails if the
committed generated output is stale.
