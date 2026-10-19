import { LightspeedClient } from "../../clients/typescript/src/client.js";
import { LightspeedRpcError } from "../../clients/typescript/src/errors.js";
import type { EventCursor } from "../../clients/typescript/src/generated/types.js";
import { readFile, writeFile, mkdir } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { monitorEventLoopDelay } from "node:perf_hooks";
import { schedule, summarize, type Sample } from "./scheduler.js";
const config = JSON.parse(await readFile(process.argv[2], "utf8")) as {
  endpoint: string;
  simulator: string;
  sessions: number;
  model: string;
  historyTurns: number;
  inputBytes: number;
  warmupTurns: number;
  mode: "runs" | "idle-readers" | "history-reads";
  rate: number;
  durationMs: number;
  deadlineMs: number;
  concurrency: number;
  readers: number;
  seed: number;
};
for (const url of [config.endpoint, config.simulator])
  if (!["127.0.0.1", "localhost", "[::1]"].includes(new URL(url).hostname))
    throw Error("This fixture runner requires loopback endpoints");
const output = process.argv[3];
if (!output)
  throw Error("usage: tsx scripts/perf/run.ts CONFIG OUTPUT_DIRECTORY");
await mkdir(output, { recursive: true });
const client = new LightspeedClient(config.endpoint);
const setupSignal = AbortSignal.timeout(30 * 60 * 1000);
const {
  result: { provider },
} = await client.call(
  "auth/providers/create",
  {
    providerId: `model:perf-${crypto.randomUUID()}`,
    config: {
      type: "modelEndpoint",
      endpoint: {
        baseUrl: config.simulator + "/v1",
        apiKinds: ["openai:completions"],
      },
    },
  },
  { signal: setupSignal },
);
const sessions: { id: string; cursor?: EventCursor; busy: boolean }[] = [];
await Promise.all(
  Array.from({ length: config.sessions }, async (_, i) => {
    const {
      result: { session },
    } = await client.call(
      "session/start",
      {
        displayName: `perf-${config.seed}-${i}`,
        config: {
          model: {
            providerId: provider.providerId.slice(6),
            apiKind: "openai:completions",
            model: config.model,
          },
          context: { compaction: { mode: "disabled" } },
        },
      },
      { signal: setupSignal },
    );
    sessions.push({ id: session.id, busy: false });
  }),
);
const text = "x".repeat(config.inputBytes);
async function turn(
  session: (typeof sessions)[number],
  signal: AbortSignal,
  sample?: Sample,
) {
  const submissionId = crypto.randomUUID();
  if (sample) {
    sample.sessionId = session.id;
    sample.submissionId = submissionId;
    sample.uncertainAdmission = true;
  }
  const accepted = await client.startRun(session.id, [{ type: "text", text }], {
    signal, submissionId,
  }).catch(error => {
    if (sample && error instanceof LightspeedRpcError) {
      sample.uncertainAdmission = false;
      sample.outcome = "failed";
    }
    throw error;
  });
  if (sample) sample.uncertainAdmission = false;
  if (sample) {
    sample.acceptedMs = performance.now();
    sample.runId = accepted.result.run.id;
    sample.sessionId = session.id;
  }
  const terminal = await client.awaitRun(session.id, accepted.result.run.id, {
    signal,
    after: session.cursor,
    waitMs: 1000,
    onEvent: (e) => {
      if (
        sample &&
        sample.firstMs === undefined &&
        e.joins.runId === accepted.result.run.id
      )
        sample.firstMs = performance.now();
    },
  });
  session.cursor = terminal.cursor;
  if (sample) {
    sample.outcome =
      terminal.state.status === "completed" ? "completed" : "failed";
    sample.terminalMs = performance.now();
    if (terminal.state.status === "failed")
      sample.error = terminal.state.message;
  } else if (terminal.state.status !== "completed")
    throw Error(`Fixture failed: ${JSON.stringify(terminal.state)}`);
}
await Promise.all(
  sessions.map(async (s) => {
    for (let i = 0; i < config.historyTurns + config.warmupTurns; i++)
      await turn(s, setupSignal);
  }),
);
await writeFile(
  output + "/fixture.json",
  JSON.stringify(
    {
      config,
      sessions,
      providerId: provider.providerId,
      preparedAt: new Date().toISOString(),
    },
    null,
    2,
  ),
);
const readerController = new AbortController();
let readerPages = 0,
  readerEvents = 0,
  readerErrors = 0;
const readers = Array.from({ length: config.readers }, async (_, i) => {
  const s = sessions[i % sessions.length];
  let cursor = s.cursor;
  try {
    while (!readerController.signal.aborted) {
      const p = await client.readEvents(s.id, {
        after: cursor,
        waitMs: 1000,
        signal: readerController.signal,
      });
      readerPages++;
      readerEvents += (p.result.events ?? []).length;
      cursor = p.result.nextCursor ?? p.result.headCursor ?? cursor;
    }
  } catch (e) {
    if (!readerController.signal.aborted) {
      readerErrors++;
      console.error(String(e));
    }
  }
});
function pgSnapshot() {
  return execFileSync(
    "docker",
    [
      "exec",
      "lightspeed-postgres",
      "psql",
      "-U",
      "lightspeed",
      "-d",
      "lightspeed",
      "--csv",
      "-c",
      "SELECT queryid,calls,total_exec_time,rows,shared_blks_hit,shared_blks_read,query FROM pg_stat_statements ORDER BY queryid;",
    ],
    { encoding: "utf8" },
  );
}
await writeFile(output + "/pg-before.csv", pgSnapshot());
const before = await (await fetch(config.simulator + "/metrics")).json();
const lag = monitorEventLoopDelay({ resolution: 10 });
lag.enable();
const cpu = process.cpuUsage();
const wallStart = new Date().toISOString();
const samples = await schedule(config, async (sample, signal) => {
  if (config.mode === "runs") {
    const s = sessions.find((s) => !s.busy);
    if (!s) {
      sample.dispatchMs = undefined;
      sample.outcome = "dropped";
      sample.error = "sessions_busy";
      return;
    }
    s.busy = true;
    try {
      await turn(s, signal, sample);
    } finally {
      // Keep uncertain or unobserved runs reserved; they may still execute.
      if (!sample.uncertainAdmission && sample.outcome !== "unfinished") s.busy = false;
    }
  } else {
    const s = sessions[sample.index % sessions.length];
    await client.readEvents(s.id, {
      after: config.mode === "idle-readers" ? s.cursor : undefined,
      waitMs: config.mode === "idle-readers" ? 1000 : 0,
      limit: 512,
      signal,
    });
    sample.terminalMs = performance.now();
    sample.outcome = "completed";
  }
});
await writeFile(output + "/pg-after.csv", pgSnapshot());
readerController.abort();
await Promise.all(readers);
lag.disable();
const after = await (await fetch(config.simulator + "/metrics")).json();
const report = {
  config,
  wallStart,
  wallEnd: new Date().toISOString(),
  summary: summarize(samples, config.durationMs),
  readers: { pages: readerPages, events: readerEvents, errors: readerErrors },
  generator: {
    cpu: process.cpuUsage(cpu),
    rss: process.memoryUsage().rss,
    eventLoopP99Ms: lag.percentile(99) / 1e6,
  },
  simulator: { before, after },
  samples,
};
await writeFile(output + "/results.json", JSON.stringify(report, null, 2));
console.log(JSON.stringify(report.summary));
if (report.summary.failed || report.summary.unfinished || readerErrors)
  process.exitCode = 2;
