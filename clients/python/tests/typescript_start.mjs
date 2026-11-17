// Used by the opt-in Python parity smoke test after building the TypeScript SDK.
import { LightspeedClient } from "../../typescript/dist/index.js";
import { createClientFromConnection } from "../../typescript/dist/node.js";

const [sessionId, submissionId, prompt] = process.argv.slice(2);
const client = process.env.LIGHTSPEED_TEST_MODEL_CONNECTION ? await createClientFromConnection({
  name: process.env.LIGHTSPEED_TEST_MODEL_CONNECTION,
  configDir: process.env.LIGHTSPEED_TEST_CONNECTION_CONFIG,
}) : new LightspeedClient({
  endpoint: process.env.LIGHTSPEED_API_URL,
  token: process.env.LIGHTSPEED_RUNTIME_TOKEN,
  universe: process.env.LIGHTSPEED_UNIVERSE,
});
const options = { signal: AbortSignal.timeout(30_000) };
await client.startSession({ sessionId, displayName: "SDK parity smoke", ...options });
const page = await client.readEvents(sessionId, options);
const { run } = await client.startRun(sessionId, prompt, { submissionId, ...options });
console.log(JSON.stringify({ runId: run.id, cursor: page.headCursor ?? null }));
