import { describe, expect, it } from "vitest";
import { sectionActivity, sectionsByRun, type RunSection } from "./run-sections";
import type { TranscriptEntry, TranscriptToolCall } from "./transcript";

const user = (key: string, runId?: string, steering = false): TranscriptEntry =>
  ({ kind: "message", key, role: "user", text: key, ...(runId ? { runId } : {}), ...(steering ? { steering } : {}) });
const assistant = (key: string, runId?: string): TranscriptEntry =>
  ({ kind: "message", key, role: "assistant", text: key, ...(runId ? { runId } : {}) });
const thinking = (key: string, runId?: string): TranscriptEntry =>
  ({ kind: "reasoning", key, text: "**Plan**", ...(runId ? { runId } : {}) });
const call = (callId: string, group: string, status: TranscriptToolCall["status"] = "succeeded"): TranscriptToolCall =>
  ({ callId, toolName: group, status, isError: status === "failed", display: { group: group as never, verb: "X" } });
const tools = (key: string, runId: string | undefined, calls: TranscriptToolCall[]): TranscriptEntry =>
  ({ kind: "tool-group", key, status: "succeeded", calls, ...(runId ? { runId } : {}) });
const summary = (key: string, runId: string, status: "completed" | "failed" | "cancelled" = "completed"): TranscriptEntry =>
  ({ kind: "run-summary", key, runId, status, usageComplete: true });
const marker = (key: string): TranscriptEntry => ({ kind: "marker", key, text: key, tone: "muted" });
const system = (key: string): TranscriptEntry => ({ kind: "system", key, text: key });

describe("run sections", () => {
  it("groups a run between its input and its summary and keeps the last reply outside the fold", () => {
    const entries = [
      user("ask", "r1"), thinking("think", "r1"), tools("batch", "r1", [call("c1", "explore")]),
      assistant("note", "r1"), assistant("reply", "r1"), summary("done", "r1"),
    ];
    expect(sectionsByRun(entries, null)).toEqual([{
      kind: "run", key: "ask", runId: "r1", input: entries[0],
      work: [entries[1], entries[2], entries[3]], reply: entries[4], summary: entries[5], live: false,
    }]);
  });

  it("keeps session markers between runs top-level and folds those raised inside a run", () => {
    const entries = [
      marker("context compacted"), user("ask", "r1"), system("Sub-agent catalog"),
      tools("batch", "r1", [call("c1", "execute")]), summary("done", "r1"), marker("session closed"),
    ];
    const sections = sectionsByRun(entries, null);
    expect(sections.map((section) => section.kind)).toEqual(["entry", "run", "entry"]);
    expect((sections[1] as RunSection).work).toEqual([entries[2], entries[3]]);
  });

  it("coalesces the instruction and catalog entries a session opens with into one chips row", () => {
    const entries = [
      system("Instructions"), system("Sub-agent catalog"), system("Bot directory"),
      marker("context compacted"), system("Bot directory (updated)"), user("ask", "r1"), summary("done", "r1"),
    ];
    expect(sectionsByRun(entries, null)).toMatchObject([
      { kind: "system", key: "Instructions", entries: [entries[0], entries[1], entries[2]] },
      { kind: "entry", key: "context compacted" },
      { kind: "system", key: "Bot directory (updated)", entries: [entries[4]] },
      { kind: "run", key: "ask" },
    ]);
  });

  it("marks the open section live only for the engine's active run", () => {
    const entries = [user("ask", "r1"), tools("batch", "r1", [call("c1", "edit")]), assistant("progress", "r1")];
    const live = sectionsByRun(entries, { runId: "r1", label: "running tools", cancelling: false });
    expect(live).toMatchObject([{ kind: "run", live: true, work: [entries[1], entries[2]] }]);
    expect((live[0] as RunSection).reply).toBeUndefined();

    const stale = sectionsByRun(entries, { runId: "r2", label: "running", cancelling: false });
    expect(stale).toMatchObject([{ kind: "run", live: false, work: [entries[1]], reply: entries[2] }]);

    const ended = sectionsByRun(entries, null);
    expect(ended).toMatchObject([{ kind: "run", live: false, reply: entries[2] }]);
    expect((ended[0] as RunSection).summary).toBeUndefined();
  });

  it("stands in an empty live section for a run that has produced nothing yet", () => {
    expect(sectionsByRun([], { runId: "r3", label: "running", cancelling: false })).toEqual([
      { kind: "run", key: "run-r3", runId: "r3", work: [], live: true },
    ]);
    const previous = [user("ask", "r1"), assistant("reply", "r1"), summary("done", "r1")];
    const sections = sectionsByRun(previous, { runId: "r2", label: "planning", cancelling: false });
    expect(sections.map((section) => section.key)).toEqual(["ask", "run-r2"]);
  });

  it("starts a section without input when the window begins mid-run and adopts the run id from the summary", () => {
    const entries = [tools("batch", undefined, [call("c1", "explore")]), assistant("reply"), summary("done", "r7")];
    expect(sectionsByRun(entries, null)).toEqual([{
      kind: "run", key: "run-at-batch", runId: "r7", work: [entries[0]], reply: entries[1], summary: entries[2], live: false,
    }]);
  });

  it("keeps a summary whose run left nothing in the window as its own section", () => {
    const entries = [summary("done", "r1"), user("ask", "r2"), assistant("reply", "r2"), summary("done-2", "r2")];
    expect(sectionsByRun(entries, null)).toMatchObject([
      { kind: "run", key: "done", runId: "r1", work: [], summary: entries[0] },
      { kind: "run", key: "ask", runId: "r2", work: [], reply: entries[2], summary: entries[3] },
    ]);
  });

  it("folds steering input into the run's work", () => {
    const entries = [
      user("ask", "r1"), tools("a", "r1", [call("c1", "execute")]), user("steer", "r1", true),
      tools("b", "r1", [call("c2", "execute")]), assistant("reply", "r1"), summary("done", "r1"),
    ];
    expect(sectionsByRun(entries, null)).toMatchObject([{
      work: [entries[1], entries[2], entries[3]], reply: entries[4],
    }]);
  });

  it("summarises tool calls, failures, thinking, and the activity families a run touched", () => {
    const section = sectionsByRun([
      user("ask", "r1"), thinking("t", "r1"),
      tools("a", "r1", [call("c1", "explore"), call("c2", "explore", "failed")]),
      tools("b", "r1", [call("c3", "bot"), { callId: "c4", toolName: "mystery", status: "succeeded", isError: false }]),
      summary("done", "r1"),
    ], null)[0] as RunSection;
    expect(sectionActivity(section)).toEqual({ toolCalls: 4, failedCalls: 1, groups: ["explore", "bot", "other"], thinking: true });
  });
});
