import { describe, expect, it } from "vitest";
import type { SessionEvent, SessionItem } from "@/api";
import { applyEvents, emptyTranscript, isFailedToolCall } from "./transcript";

type EventKind = SessionEvent["kind"];
type EventInput<T extends EventKind["type"]> =
  & { type: T }
  & Partial<Omit<Extract<EventKind, { type: T }>, "type">>;

const event = <T extends EventKind["type"]>(
  seq: number,
  kind: EventInput<T>,
): SessionEvent => ({
  cursor: { seq },
  observedAtMs: seq,
  joins: {},
  sessionId: "session-test",
  kind: {
    runId: "run-test",
    turnId: "turn-test",
    batchId: "batch-1",
    source: { type: "input", entries: [] },
    baseRevision: seq - 1,
    revision: seq,
    ...kind,
  } as unknown as Extract<EventKind, { type: T }>,
});

const item = (
  id: string,
  kind: SessionItem["kind"],
  extra: Omit<Partial<SessionItem>, "id" | "kind" | "contentRef"> = {},
): SessionItem => ({ id, kind, ...extra, contentRef: `sha256:${id}` });

describe("session transcript traces", () => {
  it("keeps readable reasoning summaries and hides opaque continuation state", () => {
    const state = applyEvents(emptyTranscript(), [
      event(1, {
        type: "contextEntriesApplied",
        entries: [
          item("reasoning-visible", { type: "reasoningState" }, {
            preview: "**Inspecting the code**\n\nI need to read the reducer first.",
          }),
          item("reasoning-opaque", { type: "reasoningState" }, {
            preview: "reasoning state rs_123",
          }),
        ],
      }),
    ]);

    expect(state.entries).toEqual([
      {
        kind: "reasoning",
        key: "reasoning-visible",
        text: "**Inspecting the code**\n\nI need to read the reducer first.",
      },
    ]);
  });

  it("merges context calls, batch metadata, results, and status into one group", () => {
    const state = applyEvents(emptyTranscript(), [
      event(1, {
        type: "contextEntriesApplied",
        entries: [
          item("tool-a", { type: "toolCall", callId: "call-a", name: "read_file" }),
          item("tool-b", { type: "toolCall", callId: "call-b", name: "exec_command" }),
        ],
      }),
      event(2, {
        type: "toolBatchStarted",
        batchId: "batch-1",
        calls: [
          {
            callId: "call-a",
            toolName: "read_file",
            argumentsRef: "sha256:args-a",
            arguments: "{\"path\":\"README.md\"}",
            display: { group: "explore", verb: "Read", target: "README.md" },
          },
          {
            callId: "call-b",
            toolName: "exec_command",
            argumentsRef: "sha256:args-b",
            arguments: "{\"argv\":[\"git\",\"status\"]}",
            display: { group: "execute", verb: "Run", target: "git status" },
          },
        ],
      }),
      event(3, { type: "toolCallCompleted", callId: "call-a", status: "succeeded" }),
      event(4, { type: "toolCallCompleted", callId: "call-b", status: "succeeded" }),
      event(5, {
        type: "contextEntriesApplied",
        entries: [
          item("result-a", { type: "toolResult", callId: "call-a", isError: false }, {
            text: "# Project",
          }),
          item("result-b", { type: "toolResult", callId: "call-b", isError: false }, {
            text: "clean",
          }),
        ],
      }),
      event(6, { type: "toolBatchCompleted", batchId: "batch-1" }),
    ]);

    expect(state.entries).toHaveLength(1);
    expect(state.entries[0]).toMatchObject({
      kind: "tool-group",
      batchId: "batch-1",
      status: "succeeded",
      calls: [
        {
          callId: "call-a",
          argumentsJson: "{\"path\":\"README.md\"}",
          output: "# Project",
          display: { group: "explore", verb: "Read", target: "README.md" },
        },
        {
          callId: "call-b",
          argumentsJson: "{\"argv\":[\"git\",\"status\"]}",
          output: "clean",
          display: { group: "execute", verb: "Run", target: "git status" },
        },
      ],
    });
  });

  it("retains failed output without treating the completed batch as a run failure", () => {
    const state = applyEvents(emptyTranscript(), [
      event(1, {
        type: "contextEntriesApplied",
        entries: [item("tool", { type: "toolCall", callId: "call", name: "web_fetch" })],
      }),
      event(2, {
        type: "contextEntriesApplied",
        entries: [
          item("result", { type: "toolResult", callId: "call", isError: true }, {
            text: "request failed",
          }),
        ],
      }),
    ]);

    expect(state.entries[0]).toMatchObject({
      kind: "tool-group",
      status: "completedWithErrors",
      calls: [{ status: "failed", output: "request failed", error: "request failed" }],
    });
  });

  it("keeps the batch and run active while sibling calls are still in progress", () => {
    const partial = applyEvents(emptyTranscript(), [
      event(1, { type: "runAccepted", runId: "5" }),
      event(2, { type: "runStarted", runId: "5" }),
      event(3, {
        type: "contextEntriesApplied",
        entries: [
          item("tool-a", { type: "toolCall", callId: "call-a", name: "read_file" }),
          item("tool-b", { type: "toolCall", callId: "call-b", name: "grep" }),
          item("tool-c", { type: "toolCall", callId: "call-c", name: "grep" }),
        ],
      }),
      event(4, {
        type: "toolBatchStarted",
        batchId: "batch-1",
        calls: [
          { callId: "call-a", toolName: "read_file", argumentsRef: "sha256:args-a" },
          { callId: "call-b", toolName: "grep", argumentsRef: "sha256:args-b" },
          { callId: "call-c", toolName: "grep", argumentsRef: "sha256:args-c" },
        ],
      }),
      event(5, { type: "toolCallStarted", callId: "call-a" }),
      event(6, { type: "toolCallStarted", callId: "call-b" }),
      event(7, { type: "toolCallStarted", callId: "call-c" }),
      event(8, { type: "toolCallCompleted", callId: "call-a", status: "succeeded" }),
      event(9, {
        type: "contextEntriesApplied",
        entries: [
          item("result-a", { type: "toolResult", callId: "call-a", isError: false }, {
            text: "contents",
          }),
        ],
      }),
      event(10, { type: "toolCallCompleted", callId: "call-b", status: "failed" }),
      event(11, {
        type: "contextEntriesApplied",
        entries: [
          item("result-b", { type: "toolResult", callId: "call-b", isError: true }, {
            text: "search timed out",
          }),
        ],
      }),
    ]);

    expect(partial.activeRun).toEqual({ runId: "5", label: "running tools" });
    expect(partial.entries[0]).toMatchObject({
      kind: "tool-group",
      status: "running",
      calls: [
        { callId: "call-a", status: "succeeded" },
        { callId: "call-b", status: "failed" },
        { callId: "call-c", status: "running" },
      ],
    });

    const complete = applyEvents(partial, [
      event(12, { type: "toolCallCompleted", callId: "call-c", status: "succeeded" }),
      event(13, {
        type: "contextEntriesApplied",
        entries: [
          item("result-c", { type: "toolResult", callId: "call-c", isError: false }, {
            text: "matches",
          }),
        ],
      }),
      event(14, { type: "toolBatchCompleted", batchId: "batch-1" }),
    ]);

    expect(complete.activeRun).toEqual({ runId: "5", label: "working" });
    expect(complete.entries[0]).toMatchObject({
      kind: "tool-group",
      status: "completedWithErrors",
    });
  });

  it("keeps a cancelled tool status neutral", () => {
    const state = applyEvents(emptyTranscript(), [
      event(1, {
        type: "contextEntriesApplied",
        entries: [item("tool", { type: "toolCall", callId: "call", name: "grep" })],
      }),
      event(2, {
        type: "toolBatchStarted",
        batchId: "batch-1",
        calls: [{ callId: "call", toolName: "grep", argumentsRef: "sha256:args" }],
      }),
      event(3, { type: "toolCallStarted", callId: "call" }),
      event(4, { type: "toolCallCompleted", callId: "call", status: "cancelled" }),
      event(5, {
        type: "contextEntriesApplied",
        entries: [
          item("result", { type: "toolResult", callId: "call", isError: true }, {
            text: "cancelled",
            display: {
              status: "cancelled",
              toolName: "grep",
              summary: { group: "explore", verb: "Search" },
            },
          }),
        ],
      }),
      event(6, { type: "toolBatchCompleted", batchId: "batch-1" }),
    ]);

    expect(state.entries[0]).toMatchObject({
      kind: "tool-group",
      status: "cancelled",
      calls: [{ status: "cancelled", isError: true }],
    });
    const group = state.entries[0];
    expect(group?.kind === "tool-group" && isFailedToolCall(group.calls[0]!)).toBe(false);
  });

  it("restores neutral cancellation from projected context alone", () => {
    const cancelledDisplay = {
      status: "cancelled" as const,
      toolName: "grep",
      summary: { group: "explore" as const, verb: "Search", target: "src" },
    };
    const state = applyEvents(emptyTranscript(), [
      event(1, {
        type: "contextStateReplaced",
        entries: [
          item("tool", { type: "toolCall", callId: "call", name: "grep" }, {
            display: {
              ...cancelledDisplay,
              arguments: "{\"pattern\":\"needle\",\"path\":\"src\"}",
            },
          }),
          item("result", { type: "toolResult", callId: "call", isError: true }, {
            text: "cancelled",
            display: cancelledDisplay,
          }),
        ],
      }),
    ]);

    expect(state.entries[0]).toMatchObject({
      kind: "tool-group",
      status: "cancelled",
      calls: [{ status: "cancelled", isError: true }],
    });
    const group = state.entries[0];
    expect(group?.kind === "tool-group" && isFailedToolCall(group.calls[0]!)).toBe(false);
  });

  it("completes a context-only tool group without batch lifecycle events", () => {
    const state = applyEvents(emptyTranscript(), [
      event(1, {
        type: "contextEntriesApplied",
        entries: [
          item("tool", { type: "toolCall", callId: "call", name: "read_file" }, {
            display: {
              toolName: "read_file",
              status: "succeeded",
              arguments: "{\"path\":\"README.md\"}",
              summary: { group: "explore", verb: "Read", target: "README.md" },
            },
          }),
        ],
      }),
    ]);

    expect(state.entries[0]).toMatchObject({
      kind: "tool-group",
      status: "succeeded",
      calls: [{ status: "succeeded" }],
    });
  });
});
