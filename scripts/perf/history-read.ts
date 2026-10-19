import { readFile, writeFile } from "node:fs/promises";
import { LightspeedClient } from "../../clients/typescript/src/client.js";
import { schedule, summarize } from "./scheduler.js";
// Reuse a prepared dataset; these reads create no history and invoke no model.
const fixture = JSON.parse(await readFile(process.argv[2], "utf8")) as {
  config: { endpoint: string };
  sessions: { id: string }[];
};
const endpoint = new URL(fixture.config.endpoint);
if (endpoint.hostname !== "127.0.0.1") throw Error("loopback fixture required");
const client = new LightspeedClient(endpoint);
const wallStart = new Date().toISOString();
const cpu = process.cpuUsage();
const samples = await schedule(
  {
    rate: Number(process.argv[3]),
    durationMs: 30000,
    deadlineMs: 60000,
    concurrency: 512,
  },
  async (s, signal) => {
    const session = fixture.sessions[s.index % fixture.sessions.length];
    s.sessionId = session.id;
    const page = await client.readEvents(session.id, {
      limit: 512,
      waitMs: 0,
      signal,
    });
    if (!page.result.events?.length)
      throw Error("expected nonempty prepared history");
    s.terminalMs = performance.now();
    s.outcome = "completed";
  },
);
await writeFile(
  process.argv[4],
  JSON.stringify(
    {
      fixture: process.argv[2],
      wallStart,
      wallEnd: new Date().toISOString(),
      cpu: process.cpuUsage(cpu),
      rss: process.memoryUsage().rss,
      summary: summarize(samples, 30000),
      samples,
    },
    null,
    2,
  ),
);

if (samples.some(s => s.outcome === 'failed' || s.outcome === 'unfinished')) process.exitCode = 2;
