import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";
import type { TranscriptRunSummary } from "@/lib/sessions/transcript";
import { TranscriptEntryView } from "./transcript-view";
import { RunOutcomeLine, RunStatsDetails, RunStatsTrigger } from "./run-stats";

const summary: TranscriptRunSummary = {
  kind: "run-summary", key: "run", runId: "run_1", status: "completed", contextTokens: 78512,
  usageComplete: true, durationMs: 84000, toolCalls: 24,
  usage: { inputTokens: 733000, outputTokens: 4200, modelCalls: 10, cachedInputTokens: 718340 },
};

function trigger(entry = summary) {
  return renderToStaticMarkup(createElement(RunStatsTrigger, { summary: entry }));
}
function outcome(entry = summary, showStatistics = true) {
  return renderToStaticMarkup(createElement(RunOutcomeLine, { summary: entry, showStatistics }));
}
function details(entry = summary) {
  return renderToStaticMarkup(createElement(RunStatsDetails, { summary: entry }));
}
function text(html: string) { return html.replace(/<[^>]+>/g, ""); }

it("offers context and usage as one popover trigger", () => {
  const html = trigger();
  expect(text(html)).toBe("Context 78.5k·Usage 737.2k");
  expect(html).not.toContain("98%");
  expect(html).toContain('aria-haspopup="dialog"');
  expect(html).toContain('aria-label="Run statistics"');
});

it("puts the outcome on one line and keeps failures and cancellations visible", () => {
  expect(text(outcome({ ...summary, status: "failed", error: "Provider unavailable" }))).toBe("Run failed: Provider unavailable1m 24sContext 78.5k·Usage 737.2k");
  expect(text(outcome({ ...summary, status: "cancelled" }))).toBe("Run cancelled1m 24sContext 78.5k·Usage 737.2k");
  expect(text(outcome())).toBe("1m 24sContext 78.5k·Usage 737.2k");
  expect(text(renderToStaticMarkup(createElement(TranscriptEntryView, { entry: summary })))).toBe("1m 24sContext 78.5k·Usage 737.2k");
  expect(outcome({ ...summary, contextTokens: undefined, usage: undefined, durationMs: undefined })).toBe("");
});

it("keeps the duration and non-success status when statistics are switched off", () => {
  expect(text(outcome(summary, false))).toBe("1m 24s");
  expect(text(outcome({ ...summary, status: "failed", error: "Provider unavailable" }, false))).toBe("Run failed: Provider unavailable1m 24s");
  expect(outcome({ ...summary, durationMs: undefined }, false)).toBe("");
});

it("separates the last context measurement from cumulative usage in the breakdown", () => {
  const content = text(details());
  expect(content).toContain("Context at last model call78.5k tokens");
  expect(content).toContain("Input733k tokens");
  expect(content).toContain("Output4.2k tokens");
  expect(content).toContain("Model calls10");
  expect(content).toContain("Tool calls24");
  expect(content).toContain("Input served from cache98%");
  expect(content).not.toContain("Run duration");
});

it("shows known context but explains why a partial run has no usage total", () => {
  const partial = { ...summary, usage: undefined, usageComplete: false };
  expect(text(trigger(partial))).toBe("Context 78.5k");
  expect(text(details(partial))).toContain("Full run usage is unavailable until earlier history is loaded.");
});

it("renders no trigger when neither figure is known and preserves explicit zero cache hits", () => {
  const missing = {
    ...summary, contextTokens: undefined,
    usage: { inputTokens: 750, outputTokens: undefined, cachedInputTokens: 0, modelCalls: 1 },
  };
  expect(trigger(missing)).toBe("");
  expect(text(outcome(missing))).toBe("1m 24s");
  const content = text(details(missing));
  expect(content).toContain("Input750 tokens");
  expect(content).toContain("OutputUnavailable");
  expect(content).toContain("Input served from cache0%");
});

it("does not invent a duration when the loaded history contains no start time", () => {
  expect(text(outcome({ ...summary, durationMs: undefined }))).toBe("Context 78.5k·Usage 737.2k");
});
