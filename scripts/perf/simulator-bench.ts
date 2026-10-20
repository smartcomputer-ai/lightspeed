import { writeFile } from "node:fs/promises";
import { schedule, summarize } from "./scheduler.js";
const rate = Number(process.argv[2]);
const output = process.argv[3];
const model = process.argv[4] ?? "perf-text";
const inputBytes = Number(process.argv[5] ?? 32);
const before = await (await fetch("http://127.0.0.1:19090/metrics")).json();
const cpu = process.cpuUsage();
const samples = await schedule(
  { rate, durationMs: 10000, deadlineMs: 10000, concurrency: 256 },
  async (s, signal) => {
    const response = await fetch("http://127.0.0.1:19090/v1/chat/completions", {
      method: "POST",
      signal,
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        model,
        messages: [
          {
            role: "user",
            content: `capacity-${s.index}` + "x".repeat(inputBytes),
          },
        ],
      }),
    });
    if (!response.ok) throw Error(`HTTP ${response.status}`);
    const body = await response.json();
    if (
      body.choices[0].message.content.length !==
      (model === "perf-history" ? 8192 : 256) ||
      body.usage.total_tokens !== (model === "perf-history" ? 10240 : 164)
    )
      throw Error("Invalid simulator response");
    s.terminalMs = performance.now();
    s.outcome = "completed";
  },
);
const after = await (await fetch("http://127.0.0.1:19090/metrics")).json();
await writeFile(
  output,
  JSON.stringify(
    {
      summary: summarize(samples, 10000),
      before,
      after,
      cpu: process.cpuUsage(cpu),
      samples,
    },
    null,
    2,
  ),
);

if (samples.some(s => s.outcome === 'failed' || s.outcome === 'unfinished')) process.exitCode = 2;
