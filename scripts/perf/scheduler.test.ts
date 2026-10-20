import { test } from "node:test";
import assert from "node:assert/strict";
import { schedule, summarize } from "./scheduler.js";
import { setTimeout as sleep } from "node:timers/promises";
test("independent arrivals stay scheduled when concurrency is exhausted", async () => {
  const rows = await schedule(
    { rate: 1000, durationMs: 20, deadlineMs: 100, concurrency: 1 },
    async (s) => {
      await sleep(30);
      s.outcome = "completed";
      s.terminalMs = performance.now();
    },
  );
  assert.equal(rows.length, 20);
  assert.equal(rows.filter((s) => s.outcome === "completed").length, 1);
  assert.equal(rows.filter((s) => s.outcome === "dropped").length, 19);
  const totals = summarize(rows, 20);
  assert.equal(totals.submitted, 1);
  assert.ok(totals.drainMs > 0);
});
test("deadline distinguishes unresolved work from errors", async () => {
  const rows = await schedule(
    { rate: 100, durationMs: 30, deadlineMs: 10, concurrency: 10 },
    async (s, signal) => {
      if (s.index === 0) throw Error("rejected");
      s.acceptedMs = performance.now();
      await sleep(1000, undefined, { signal });
    },
  );
  assert.equal(rows[0].outcome, "failed");
  assert.equal(rows[1].outcome, "unfinished");
  const t = summarize(rows, 30);
  assert.equal(t.completed, 0);
  assert.equal(t.failed + t.unfinished + t.dropped, 3);
});
test("invalid plans fail explicitly", async () => {
  await assert.rejects(
    schedule(
      { rate: 0, durationMs: 1, deadlineMs: 1, concurrency: 1 },
      async () => { },
    ),
  );
});
test("completed counts and latency do not confuse acceptance with completion", () => {
  const summary = summarize(
    [
      {
        index: 0,
        scheduledMs: 0,
        dispatchMs: 1,
        acceptedMs: 2,
        terminalMs: 15,
        outcome: "completed",
      },
      {
        index: 1,
        scheduledMs: 5,
        dispatchMs: 6,
        acceptedMs: 7,
        outcome: "unfinished",
      },
    ],
    10,
  );
  assert.equal(summary.accepted, 2);
  assert.equal(summary.completed, 1);
  assert.equal(summary.completedDuringArrivalsPerSecond, 0);
  assert.equal(summary.peakOutstanding, 2);
  assert.equal(summary.terminalMs.p50, 15);
});
test("transport loss after acceptance remains unfinished", async () => {
  const rows = await schedule(
    { rate: 1, durationMs: 10, deadlineMs: 100, concurrency: 1 },
    async (s) => {
      s.acceptedMs = performance.now();
      throw Error("event read connection lost");
    },
  );
  assert.equal(rows[0].outcome, "unfinished");
  assert.ok(rows[0].settledMs);
  assert.equal(summarize(rows, 10).accepted, 1);
});
test("an ambiguous admission is unfinished, not rejected", async () => {
  const rows = await schedule({ rate: 1, durationMs: 10, deadlineMs: 100, concurrency: 1 }, async s => {
    s.uncertainAdmission = true;
    throw Error("admission response lost");
  });
  assert.equal(rows[0].outcome, "unfinished");
});
test("drain includes failed requests settling after arrivals stop", () => {
  const result = summarize([{ index: 0, scheduledMs: 0, dispatchMs: 1, settledMs: 20, outcome: "failed" }], 10);
  assert.equal(result.drainMs, 10);
});
