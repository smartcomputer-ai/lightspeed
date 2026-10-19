// Validate a persisted session across a service/driver restart, outside load windows.
import { readFile, writeFile } from "node:fs/promises";
import { LightspeedClient } from "../../clients/typescript/src/client.js";
import type { EventCursor } from "../../clients/typescript/src/generated/types.js";

const fixture = JSON.parse(await readFile(process.argv[2], "utf8")) as {
  config: { endpoint: string; simulator: string };
  sessions: { id: string; cursor?: EventCursor }[];
};
for (const url of [fixture.config.endpoint, fixture.config.simulator]) {
  if (!["127.0.0.1", "localhost", "[::1]"].includes(new URL(url).hostname)) {
    throw Error("Continuity validation requires local synthetic fixtures");
  }
}
const session = fixture.sessions[0];
if (!session || !process.argv[3]) throw Error("usage: tsx resume-fixture.ts FIXTURE OUTPUT");
const client = new LightspeedClient(fixture.config.endpoint);
const signal = AbortSignal.timeout(60_000);
const started = new Date().toISOString();
const accepted = await client.startRun(session.id, [{ type: "text", text: "synthetic continuity check" }], {
  submissionId: crypto.randomUUID(), signal,
});
const terminal = await client.awaitRun(session.id, accepted.result.run.id, {
  signal, after: session.cursor, waitMs: 1000,
});
await writeFile(process.argv[3], JSON.stringify({
  started, finished: new Date().toISOString(), sessionId: session.id,
  runId: accepted.result.run.id, status: terminal.state.status, cursor: terminal.cursor,
}, null, 2));
if (terminal.state.status !== "completed") throw Error(`Continuity run ${terminal.state.status}`);
