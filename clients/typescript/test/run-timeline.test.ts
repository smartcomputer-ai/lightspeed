import { describe, expect, it } from "vitest";
import { runTimeline, type ContextEntryView, type RunView, type ToolCallView } from "../src/index.js";

const entry = (id: string, kind: ContextEntryView["kind"]): ContextEntryView => ({
  id,
  kind,
  content: { blobRef: `blob_${id}`, mime: "text/plain", providerKind: "text" },
});
const call = (callId: string, toolName = "tool"): ToolCallView => ({
  callId,
  toolName,
  argumentsRef: `args_${callId}`,
  status: "succeeded",
});
const run = (fields: Partial<RunView>): RunView => ({
  id: "run_1",
  status: "completed",
  source: { type: "input", items: [] },
  ...fields,
});
const shape = (view: RunView) =>
  runTimeline(view).map((item) => (item.type === "tool" ? `tool:${item.callId}` : item.type === "message" ? item.role : item.type));

describe("runTimeline", () => {
  it("merges each call and result with its view, in the order they happened", () => {
    const timeline = runTimeline(
      run({
        entries: [
          entry("1", { type: "message", role: "user" }),
          entry("2", { type: "toolCall", callId: "a", name: "from_entry" }),
          entry("3", { type: "toolResult", callId: "a", isError: false }),
          entry("4", { type: "message", role: "assistant" }),
        ],
        toolBatches: [{ id: "batch_1", turnId: "1", status: "succeeded", calls: [call("a", "from_view")] }],
      }),
    );

    expect(timeline.map((item) => item.type)).toEqual(["message", "tool", "message"]);
    expect(timeline[1]).toMatchObject({
      type: "tool",
      callId: "a",
      name: "from_view",
      batchId: "batch_1",
      call: { status: "succeeded" },
      callEntry: { id: "2" },
      resultEntry: { id: "3" },
    });
  });

  it("keeps parallel calls apart when their results arrive together", () => {
    const view = run({
      entries: [
        entry("1", { type: "toolCall", callId: "a", name: "a" }),
        entry("2", { type: "toolCall", callId: "b", name: "b" }),
        entry("3", { type: "toolResult", callId: "b", isError: true }),
        entry("4", { type: "toolResult", callId: "a", isError: false }),
      ],
      toolBatches: [{ id: "batch_1", turnId: "1", status: "succeeded", calls: [call("a"), call("b")] }],
    });

    const timeline = runTimeline(view);
    expect(shape(view)).toEqual(["tool:a", "tool:b"]);
    expect(timeline[0]).toMatchObject({ resultEntry: { id: "4" } });
    expect(timeline[1]).toMatchObject({ resultEntry: { id: "3" } });
  });

  it("puts a call still in flight last, with no entries", () => {
    const view = run({
      status: "running",
      entries: [
        entry("1", { type: "message", role: "user" }),
        entry("2", { type: "toolCall", callId: "a", name: "a" }),
      ],
      toolBatches: [{ id: "batch_1", turnId: "1", status: "running", calls: [call("a"), call("b", "pending")] }],
    });

    expect(shape(view)).toEqual(["user", "tool:a", "tool:b"]);
    expect(runTimeline(view)[2]).toMatchObject({ name: "pending", callEntry: null, resultEntry: null });
  });

  it("keeps a call the run has no view of, and a result without its call", () => {
    const timeline = runTimeline(
      run({
        entries: [
          entry("1", { type: "toolCall", callId: "hosted", name: "web_search" }),
          entry("2", { type: "toolResult", callId: "lost", isError: false }),
        ],
      }),
    );

    expect(timeline[0]).toMatchObject({ name: "web_search", call: null, batchId: null, callEntry: { id: "1" } });
    expect(timeline[1]).toMatchObject({ name: "unknown", callEntry: null, resultEntry: { id: "2" } });
  });

  it("classifies reasoning and keeps other entries visible", () => {
    const view = run({
      entries: [
        entry("1", { type: "reasoningState" }),
        entry("2", { type: "instructions" }),
        entry("3", { type: "catalog", title: "Skills" }),
        entry("4", { type: "providerOpaque" }),
      ],
    });
    expect(shape(view)).toEqual(["reasoning", "other", "other", "other"]);
  });

  it("is empty for a run that has not produced anything", () => {
    expect(runTimeline(run({ status: "queued" }))).toEqual([]);
  });
});
