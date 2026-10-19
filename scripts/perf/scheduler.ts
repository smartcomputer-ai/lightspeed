import { setTimeout as sleep } from "node:timers/promises";
export type Sample = {
  index: number;
  sessionId?: string;
  runId?: string;
  submissionId?: string;
  uncertainAdmission?: boolean;
  scheduledMs: number;
  dispatchMs?: number;
  acceptedMs?: number;
  firstMs?: number;
  terminalMs?: number;
  settledMs?: number;
  outcome: "dropped" | "completed" | "failed" | "unfinished";
  error?: string;
};
export type Plan = {
  rate: number;
  durationMs: number;
  deadlineMs: number;
  concurrency: number;
};
export async function schedule(
  plan: Plan,
  work: (sample: Sample, signal: AbortSignal) => Promise<void>,
): Promise<Sample[]> {
  for (const n of [
    plan.rate,
    plan.durationMs,
    plan.deadlineMs,
    plan.concurrency,
  ])
    if (!Number.isFinite(n) || n <= 0)
      throw Error("positive finite plan values required");
  if (!Number.isInteger(plan.concurrency))
    throw Error("integer concurrency required");
  const start = performance.now();
  const samples: Sample[] = [];
  const pending = new Set<Promise<void>>();
  const controller = new AbortController();
  const deadline = setTimeout(
    () => controller.abort(),
    plan.durationMs + plan.deadlineMs,
  );
  try {
    for (let i = 0; (i * 1000) / plan.rate < plan.durationMs; i++) {
      const scheduledMs = start + (i * 1000) / plan.rate;
      while (performance.now() < scheduledMs)
        await sleep(Math.max(1, scheduledMs - performance.now()));
      const sample: Sample = { index: i, scheduledMs, outcome: "dropped" };
      samples.push(sample);
      if (
        pending.size >= plan.concurrency ||
        controller.signal.aborted ||
        performance.now() >= start + plan.durationMs
      ) {
        sample.error =
          pending.size >= plan.concurrency
            ? "concurrency_limit"
            : "arrival_window_elapsed";
        continue;
      }
      sample.dispatchMs = performance.now();
      sample.outcome = "unfinished";
      const task = work(sample, controller.signal)
        .catch((e) => {
          sample.error = String(e);
          sample.outcome =
            controller.signal.aborted || sample.uncertainAdmission || sample.acceptedMs !== undefined
              ? "unfinished"
              : "failed";
        })
        .finally(() => {
          sample.settledMs = performance.now();
          pending.delete(task);
        });
      pending.add(task);
    }
    await Promise.all(pending);
    return samples;
  } finally {
    clearTimeout(deadline);
  }
}
export function quantiles(values: number[]) {
  const v = [...values].sort((a, b) => a - b);
  return Object.fromEntries(
    [50, 95, 99].map((p) => [
      `p${p}`,
      v.length ? v[Math.ceil((v.length * p) / 100) - 1] : null,
    ]),
  );
}
export function summarize(samples: Sample[], durationMs: number) {
  const count = (status: Sample["outcome"]) =>
    samples.filter((s) => s.outcome === status).length;
  const submitted = samples.filter((s) => s.dispatchMs !== undefined).length;
  const completed = count("completed");
  const transitions = samples
    .flatMap((s) =>
      s.dispatchMs === undefined
        ? []
        : [
          [s.dispatchMs, 1],
          ...((s.terminalMs ?? s.settledMs) === undefined
            ? []
            : [[(s.terminalMs ?? s.settledMs)!, -1]]),
        ],
    )
    .sort((a, b) => a[0] - b[0]);
  let outstanding = 0,
    peakOutstanding = 0;
  for (const [, change] of transitions) {
    outstanding += change;
    peakOutstanding = Math.max(outstanding, peakOutstanding);
  }
  const latency = (key: "acceptedMs" | "firstMs" | "terminalMs") =>
    quantiles(
      samples.flatMap((s) =>
        s[key] === undefined ? [] : [s[key]! - s.scheduledMs],
      ),
    );
  const start = samples[0]?.scheduledMs ?? 0;
  const end = Math.max(
    start + durationMs,
    ...samples.map((s) => s.settledMs ?? s.terminalMs ?? 0),
  );
  return {
    peakOutstanding,
    scheduled: samples.length,
    submitted,
    accepted: samples.filter((s) => s.acceptedMs !== undefined).length,
    completed,
    failed: count("failed"),
    unfinished: count("unfinished"),
    dropped: count("dropped"),
    offeredPerSecond: (samples.length * 1000) / durationMs,
    submittedPerSecond: (submitted * 1000) / durationMs,
    completedDuringArrivalsPerSecond:
      (samples.filter(
        (s) => s.outcome === "completed" && s.terminalMs! < start + durationMs,
      ).length *
        1000) /
      durationMs,
    completedIncludingDrainPerSecond: (completed * 1000) / (end - start),
    drainMs: end - start - durationMs,
    admissionMs: latency("acceptedMs"),
    firstProgressMs: latency("firstMs"),
    terminalMs: latency("terminalMs"),
    dispatchLagMs: quantiles(
      samples.flatMap((s) =>
        s.dispatchMs === undefined ? [] : [s.dispatchMs - s.scheduledMs],
      ),
    ),
  };
}
