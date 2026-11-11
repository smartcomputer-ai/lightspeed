// Used by the opt-in Python parity smoke test after building the TypeScript SDK.
import { LightspeedClient } from "../../typescript/dist/index.js";

const [sessionId, submissionId, prompt] = process.argv.slice(2);
const client = new LightspeedClient({
  endpoint: process.env.LIGHTSPEED_API_URL,
  apiKey: process.env.LIGHTSPEED_API_KEY,
  universe: process.env.LIGHTSPEED_UNIVERSE,
});
const options = { signal: AbortSignal.timeout(30_000) };
await client.startSession({ sessionId, displayName: "SDK parity smoke", ...options });
const page = await client.readEvents(sessionId, options);
const { run } = await client.startRun(sessionId, prompt, { submissionId, ...options });
console.log(JSON.stringify({ runId: run.id, cursor: page.headCursor ?? null }));
