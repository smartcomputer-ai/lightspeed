import { describe, expect, it } from "vitest";
import type { SessionEvent, SessionItem } from "@/api";
import {
  applyEvents,
  emptyTranscript,
  formatDuration,
  formatTokens,
  isFailedToolCall,
  reconcileRuns,
  runInProgress,
  type TranscriptEntry,
} from "./transcript";
import type { SessionRunView } from "@/api";
import { TranscriptWindow } from "./transcript-window";

type EventKind = SessionEvent["kind"];
type EventInput<T extends EventKind["type"]> =
  & { type: T }
  & Partial<Omit<Extract<EventKind, { type: T }>, "type">>;

const event = <T extends EventKind["type"]>(
  seq: number,
  kind: EventInput<T>,
  observedAtMs = seq,
): SessionEvent => ({
  cursor: { seq },
  observedAtMs,
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
  extra: Omit<Partial<SessionItem>, "id" | "kind"> = {},
): SessionItem => ({ id, kind, content: { contentRef: `sha256:${id}`, mediaType: null, providerKind: null }, ...extra });

const runView = (
  id: string,
  status: SessionRunView["status"],
  text = "",
): SessionRunView => ({
  id,
  status,
  acceptedAtMs: 0,
  source: { type: "input", preview: text || null, previewTruncated: false },
});

describe("transcript history windows", () => {
  it("retains the completion entry when a newer session snapshot arrives before its event", () => {
    const window = new TranscriptWindow();
    window.append([event(1, { type: "runStarted" })]);
    window.reconcile([runView("run-test", "completed")]);
    window.append([event(2, { type: "runCompleted" })]);
    window.append([event(2, { type: "runCompleted" })]);
    expect(window.state.entries).toMatchObject([{ kind: "run-summary", key: "evt-2", status: "completed", durationMs: 1 }]);
  });
  it("renders a result whose call began before the window, then hydrates it without regressing live controls", () => {
    const events = [
      event(1, { type: "runStarted" }),
      event(2, { type: "contextEntriesApplied", entries: [item("input", { type: "message", role: "user" }, { text: "Do work" })] }),
      event(3, { type: "toolBatchStarted", calls: [{ callId: "call", toolName: "exec_command", toolId: "env.run_process", argumentsRef: "sha256:args", arguments: '{"command":"pwd"}' }] }),
      event(4, { type: "toolCallCompleted", callId: "call", status: "succeeded" }),
      event(5, { type: "contextEntriesApplied", entries: [item("result", { type: "toolResult", callId: "call", isError: false }, {
        text: "/workspace", source: { type: "tool", runId: "run-test", turnId: "turn-test", batchId: "batch-1" },
      })] }),
      event(6, { type: "runCompleted" }),
    ];
    const window = new TranscriptWindow();
    window.append(events.slice(4));
    const initial = window.state.entries[0]!;
    expect(initial).toMatchObject({ kind: "tool-group", calls: [{ output: "/workspace", continuation: true }] });
    const revision = window.state.runRevision;
    window.prepend(events.slice(2, 4));
    expect(window.state.entries[0]).toMatchObject({ key: initial.key, calls: [{ toolName: "exec_command", output: "/workspace", status: "succeeded" }] });
    window.prepend(events.slice(0, 2));
    expect(window.state.activeRun).toBeNull();
    expect(window.state.runRevision).toBe(revision);
    expect(window.state.runPhases.get("run-test")).toBe("terminal");
    expect(window.state.entries).toEqual(applyEvents(emptyTranscript(), events).entries);
    window.prepend(events); // Overlapping retries neither duplicate nor count usage twice.
    expect(window.state.entries).toHaveLength(3);
  });

  it("does not report partial token totals as a complete run summary", () => {
    const window = new TranscriptWindow();
    window.append([event(5, { type: "turnGenerationCompleted", usage: { inputTokens: 200, outputTokens: 20 } })]);
    window.reconcile([{ ...runView("run-test", "running"), startedAtMs: 1 }]);
    window.append([event(6, { type: "runCompleted" })]);
    expect(window.state.entries.at(-1)).toMatchObject({ kind: "run-summary", durationMs: 5, contextTokens: 200, usage: undefined, usageComplete: false });
    window.prepend([event(4, { type: "turnPlanned" })]);
    expect(window.state.entries.at(-1)).toMatchObject({ durationMs: 5, contextTokens: 200, usage: undefined, usageComplete: false });
    window.prepend([
      event(1, { type: "runStarted" }),
      event(2, { type: "turnGenerationCompleted", usage: { inputTokens: 100, outputTokens: 10 } }),
    ]);
    expect(window.state.entries.at(-1)).toMatchObject({ contextTokens: 200, usage: { inputTokens: 300, outputTokens: 30, modelCalls: 2 }, usageComplete: true });
  });

  it("can reconstruct a single run spanning more than the old catch-up cap", () => {
    const events = [event(1, { type: "runStarted" }), ...Array.from({ length: 10_500 }, (_, index) =>
      event(index + 2, { type: "contextEntriesApplied", entries: [item(`message-${index}`, { type: "message", role: "assistant" }, { text: `Reply ${index}` })] })),
      event(10_502, { type: "runCompleted" })];
    const window = new TranscriptWindow();
    window.append(events.slice(-500));
    expect(window.state.entries.some((entry) => entry.key === "message-10499")).toBe(true);
    for (let end = events.length - 500; end > 0; end -= 500) window.prepend(events.slice(Math.max(0, end - 500), end));
    expect(window.state.entries).toEqual(applyEvents(emptyTranscript(), events).entries);
  });
});

describe("session transcript traces", () => {
  it.each(["context first", "batch first"])("keeps registry IDs separate from model names (%s)", (order) => {
    const calls = [
      { callId: "responses", toolId: "env.run_process", toolName: "exec_command", argumentsRef: "sha256:args-1" },
      { callId: "anthropic", toolId: "env.run_process", toolName: "Bash", argumentsRef: "sha256:args-2" },
      { callId: "external", toolId: "custom_function", toolName: "custom_function", argumentsRef: "sha256:args-3" },
      { callId: "unavailable", toolName: "unknown_alias", argumentsRef: "sha256:args-4" },
    ];
    const batch = { type: "toolBatchStarted" as const, calls };
    const context = {
      type: "contextEntriesApplied" as const,
      entries: calls.map((call) => item(`entry-${call.callId}`, {
        type: "toolCall", callId: call.callId, name: call.toolName,
      })),
    };
    const ordered = order === "context first" ? [context, batch] : [batch, context];
    const events = [
      ...ordered.map((kind, i) => event(i + 1, kind)),
      event(3, {
        type: "contextEntriesApplied",
        entries: calls.map((call) => item(`result-${call.callId}`, {
          type: "toolResult", callId: call.callId, isError: call.callId === "unavailable",
        }, { text: call.callId === "unavailable" ? "Tool unavailable" : "done" })),
      }),
      event(4, { type: "toolBatchCompleted" }),
    ];
    const state = applyEvents(emptyTranscript(), events);
    const streamed = events.reduce((previous, next) => applyEvents(previous, [next]), emptyTranscript());
    expect(streamed.entries).toEqual(state.entries);
    expect(state.entries).toHaveLength(1);
    expect(state.entries[0]).toMatchObject({
      kind: "tool-group",
      status: "completedWithErrors",
      calls: calls.map(({ callId, toolName, ...rest }) => ({
        callId,
        toolId: "toolId" in rest ? rest.toolId : undefined,
        toolName,
        status: callId === "unavailable" ? "failed" : "succeeded",
      })),
    });
  });

  it("keeps full messages and blob references for truncated tool results", () => {
    const text = "A full message 🦀. ".repeat(700);
    const state = applyEvents(emptyTranscript(), [
      event(1, {
        type: "contextEntriesApplied",
        entries: [
          item("message", { type: "message", role: "assistant" }, {
            text,
            content: { contentRef: "sha256:message", mediaType: "application/json", providerKind: "anthropic.messages.text_blocks" },
          }),
          item("tool", { type: "toolCall", callId: "call", name: "read_file" }),
          item("result", { type: "toolResult", callId: "call", isError: false }, {
            text: "result preview",
            textTruncated: true,
          }),
        ],
      }),
    ]);

    expect(state.entries[0]).toMatchObject({
      kind: "message",
      text,
    });
    expect(state.entries[1]).toMatchObject({
      kind: "tool-group",
      calls: [{
        output: "result preview",
        outputContentRef: "sha256:result",
        outputTruncated: true,
      }],
    });
  });

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

  it("uses full projected reasoning text instead of its preview", () => {
    const text = "Complete projected reasoning 🦀. ".repeat(700) + "Done.";
    const content = { contentRef: "sha256:reasoning", mediaType: "application/json", providerKind: "anthropic.messages.thinking" };
    const state = applyEvents(emptyTranscript(), [event(1, {
      type: "contextEntriesApplied",
      entries: [item("reasoning", { type: "reasoningState" }, {
        content, preview: "short preview", text,
      })],
    })]);
    expect(state.entries).toEqual([{
      kind: "reasoning", key: "reasoning", text,
    }]);
  });

  it("renders citations projected on an assistant message", () => {
    const state = applyEvents(emptyTranscript(), [
      event(1, {
        type: "contextEntriesApplied",
        entries: [
          item("answer", { type: "message", role: "assistant" }, {
            text: "A sourced answer.",
            citations: [{
              url: "https://example.com/source",
              title: "Example source",
              citedText: "A sourced answer",
            }],
          }),
          item("search-result", { type: "providerOpaque" }),
        ],
      }),
    ]);

    expect(state.entries).toEqual([{
      kind: "message",
      key: "answer",
      role: "assistant",
      text: "A sourced answer.",
      citations: [{
        url: "https://example.com/source",
        title: "Example source",
        citedText: "A sourced answer",
      }],
    }]);
  });

  it("ignores opaque entries that carry no display", () => {
    const state = applyEvents(emptyTranscript(), [
      event(1, {
        type: "contextEntriesApplied",
        entries: [
          item("answer", { type: "message", role: "assistant" }, {
            text: "A fetched answer.",
            citations: [{
              url: "https://example.com/fetched",
              title: "Fetched source",
              citedText: "A fetched answer",
            }],
          }),
          item("server-tool-use", { type: "providerOpaque" }),
          item("server-tool-result", { type: "providerOpaque" }),
        ],
      }),
    ]);

    expect(state.entries).toEqual([{
      kind: "message",
      key: "answer",
      role: "assistant",
      text: "A fetched answer.",
      citations: [{
        url: "https://example.com/fetched",
        title: "Fetched source",
        citedText: "A fetched answer",
      }],
    }]);
  });

  it("shows native compaction once across repeated context events without its payload", () => {
    const compacted = item("native-compaction", { type: "providerOpaque" }, {
      content: {
        contentRef: "sha256:compaction",
        mediaType: "application/json",
        providerKind: "openai.responses.compaction",
      },
      text: '{"encrypted_content":"hidden-encrypted-payload"}',
      preview: "hidden-encrypted-preview",
    });
    const events = [
      event(1, { type: "contextEntriesApplied", entries: [compacted] }),
      event(2, { type: "contextKeyPrefixReplaced", entries: [compacted] }),
      event(3, { type: "contextStateReplaced", entries: [compacted] }),
    ];
    const state = applyEvents(emptyTranscript(), events);
    const streamed = events.reduce((previous, next) => applyEvents(previous, [next]), emptyTranscript());
    expect(streamed.entries).toEqual(state.entries);
    expect(state.entries).toEqual([{
      kind: "marker", key: "native-compaction", text: "context compacted", tone: "muted",
    }]);
    expect(JSON.stringify(state.entries)).not.toContain("hidden-encrypted");
  });

  it("preserves the standalone compaction marker", () => {
    const state = applyEvents(emptyTranscript(), [
      event(1, { type: "contextCompactionFinished", status: "succeeded" }),
    ]);
    expect(state.entries).toEqual([{
      kind: "marker", key: "evt-1", text: "context compacted", tone: "muted",
    }]);
  });

  it("preserves opaque tool displays while hiding unrelated opaque payloads", () => {
    const state = applyEvents(emptyTranscript(), [event(1, {
      type: "contextEntriesApplied",
      entries: [
        item("search", { type: "providerOpaque" }, {
          display: {
            toolName: "web_search", status: "succeeded",
            arguments: "query", output: "search result", isError: false,
            summary: { group: "explore", verb: "Search", target: "query" },
          },
        }),
        item("unrelated", { type: "providerOpaque" }, {
          content: {
            contentRef: "sha256:unrelated", mediaType: "application/json",
            providerKind: "unrelated.compaction",
          },
          text: "hidden opaque text", preview: "context compacted",
        }),
      ],
    })]);
    expect(state.entries).toHaveLength(1);
    expect(state.entries[0]).toMatchObject({
      kind: "tool-group", key: "search", status: "succeeded",
      calls: [{
        callId: "search", toolName: "web_search", status: "succeeded",
        argumentsJson: "query", output: "search result", isError: false,
      }],
    });
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

    expect(partial.activeRun).toMatchObject({
      runId: "5",
      label: "running tools",
      cancelling: false,
    });
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

    expect(complete.activeRun).toMatchObject({ runId: "5", label: "working", cancelling: false });
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

describe("session transcript run control", () => {
  it("queues runs accepted behind the active one and starts them in order", () => {
    let state = applyEvents(emptyTranscript(), [
      event(1, { type: "runAccepted", runId: "run_1" }),
      event(2, { type: "runStarted", runId: "run_1" }),
      event(3, { type: "runAccepted", runId: "run_2" }),
      event(4, { type: "runAccepted", runId: "run_3" }),
    ]);
    expect(state.activeRun).toMatchObject({ runId: "run_1", label: "running", cancelling: false });
    expect(state.queuedRuns).toEqual([{ runId: "run_2" }, { runId: "run_3" }]);
    expect(runInProgress(state)).toBe(true);

    state = applyEvents(state, [
      event(5, { type: "runCancelled", runId: "run_2" }),
      event(6, { type: "runCompleted", runId: "run_1" }),
      event(7, { type: "runStarted", runId: "run_3" }),
    ]);
    expect(state.activeRun).toMatchObject({ runId: "run_3", label: "running", cancelling: false });
    expect(state.queuedRuns).toEqual([]);
    expect(state.entries).toEqual([
      { kind: "marker", key: "evt-5", text: "queued message cancelled", tone: "muted" },
      { kind: "run-summary", key: "evt-6", runId: "run_1", status: "completed", durationMs: 4, contextTokens: undefined, usage: undefined, usageComplete: true, toolCalls: 0 },
    ]);

    state = applyEvents(state, [event(8, { type: "runCompleted", runId: "run_3" })]);
    expect(state.activeRun).toBeNull();
    expect(runInProgress(state)).toBe(false);
  });

  it("maps client submission ids to run ids on acceptance", () => {
    const state = applyEvents(emptyTranscript(), [
      event(1, { type: "runAccepted", runId: "run_1", submissionId: "sub-a" }),
      event(2, { type: "runStarted", runId: "run_1" }),
      event(3, { type: "runAccepted", runId: "run_2", submissionId: "sub-b" }),
    ]);
    expect(state.runBySubmission.get("sub-a")).toBe("run_1");
    expect(state.runBySubmission.get("sub-b")).toBe("run_2");
    expect(state.runPhases.get("run_2")).toBe("queued");
  });

  it("marks the active run cancelling until the terminal event lands", () => {
    let state = applyEvents(emptyTranscript(), [
      event(1, { type: "runAccepted", runId: "run_1" }),
      event(2, { type: "runStarted", runId: "run_1" }),
      event(3, { type: "turnStarted", runId: "run_1" }),
      event(4, { type: "runCancellationRequested", runId: "run_1" }),
      // Lifecycle labels no longer override "cancelling".
      event(5, { type: "turnCancelled", runId: "run_1" }),
    ]);
    expect(state.activeRun).toMatchObject({ runId: "run_1", label: "cancelling", cancelling: true });

    state = applyEvents(state, [event(6, { type: "runCancelled", runId: "run_1" })]);
    expect(state.activeRun).toBeNull();
    expect(state.entries.at(-1)).toMatchObject({
      kind: "run-summary", key: "evt-6", status: "cancelled", durationMs: 4,
    });
  });

  it("keeps approval-parked runs active and refreshes their projected cards", () => {
    let state = applyEvents(emptyTranscript(), [
      event(1, { type: "runAccepted", runId: "run_1" }),
      event(2, { type: "runStarted", runId: "run_1" }),
      event(3, {
        type: "approvalRequested",
        runId: "run_1",
        approvalId: "approval_1",
        subject: {
          kind: "mcpToolCall",
          serverId: "mail",
          serverLabel: "Mail",
          toolName: "send",
          argumentsRef: "sha256:args",
          argumentsPreview: "{}",
        },
      }),
      event(4, { type: "approvalRunParked", runId: "run_1" }),
    ]);
    expect(state.activeRun).toMatchObject({
      runId: "run_1",
      label: "approval required",
      cancelling: false,
    });
    expect(runInProgress(state)).toBe(true);

    state = reconcileRuns(state, [runView("run_1", "parked")]);
    expect(state.activeRun?.label).toBe("approval required");

    state = applyEvents(state, [
      event(5, {
        type: "approvalDecided",
        runId: "run_1",
        approvalId: "approval_1",
        decision: "approve",
      }),
    ]);
    expect(state.activeRun).not.toBeNull();
    expect(state.runRevision).toBeGreaterThan(2);
  });

  it("folds steering messages as tagged user entries on their run", () => {
    const state = applyEvents(emptyTranscript(), [
      event(1, {
        type: "contextEntriesApplied",
        entries: [
          item("input", { type: "message", role: "user" }, {
            text: "do the task",
            source: { type: "runInput", runId: "run_1", inputIndex: 0 },
          }),
          item("steer", { type: "message", role: "user" }, {
            text: "also mention the moon",
            origin: "user:operator",
            source: { type: "steering", runId: "run_1", steeringId: "steering_1", inputIndex: 0 },
          }),
        ],
      }),
    ]);
    expect(state.entries).toEqual([
      {
        kind: "message",
        key: "input",
        role: "user",
        text: "do the task",
        runId: "run_1",
      },
      {
        kind: "message",
        key: "steer",
        role: "user",
        text: "also mention the moon",
        runId: "run_1",
        steering: true,
        origin: "user:operator",
      },
    ]);
  });

  it("reconciles the authoritative session view forward only", () => {
    // The tail missed the start (truncated catch-up): the snapshot seeds it.
    let state = reconcileRuns(emptyTranscript(), [
      runView("run_1", "completed"),
      runView("run_2", "running"),
      runView("run_3", "queued", "later"),
    ]);
    expect(state.activeRun).toMatchObject({ runId: "run_2", label: "running", cancelling: false });
    expect(state.queuedRuns).toEqual([{ runId: "run_3" }]);

    // A stale snapshot never regresses what the tail already knows.
    state = applyEvents(state, [event(9, { type: "runCompleted", runId: "run_2" })]);
    const stale = reconcileRuns(state, [runView("run_2", "running")]);
    expect(stale).toBe(state);
    expect(stale.activeRun).toBeNull();

    // A terminal status in the snapshot heals a missed terminal event.
    const healed = reconcileRuns(state, [runView("run_3", "cancelled")]);
    expect(healed.queuedRuns).toEqual([]);
    expect(runInProgress(healed)).toBe(false);

    // Identity is preserved when nothing changes.
    expect(reconcileRuns(healed, [runView("run_3", "cancelled")])).toBe(healed);
  });
});

describe("run statistics", () => {
  it("includes provider-native tools once while excluding compaction entries", () => {
    const source = { type: "assistantOutput" as const, runId: "run-test", turnId: "turn-test" };
    const entries = [
      item("search", { type: "providerOpaque" }, {
        source, display: { toolName: "web_search", status: "succeeded", summary: { group: "explore", verb: "Search" } },
      }),
      item("compaction", { type: "providerOpaque" }, {
        source, content: { contentRef: "sha256:compact", mediaType: null, providerKind: "openai.responses.compaction" },
      }),
    ];
    const state = applyEvents(emptyTranscript(), [
      event(1, { type: "runStarted" }),
      event(2, { type: "contextEntriesApplied", entries }),
      event(3, { type: "contextStateReplaced", entries }),
      event(4, { type: "runCompleted" }),
    ]);
    expect(state.entries.at(-1)).toMatchObject({ toolCalls: 1 });
  });
  it("counts unique tool calls across batches, outcomes, context copies, and older pages", () => {
    const calls = ["a", "b"].map((callId) => ({ callId, toolName: "exec_command", toolId: "env.run_process", argumentsRef: `sha256:${callId}` }));
    const events = [
      event(1, { type: "runStarted" }),
      event(2, { type: "toolBatchStarted", calls }),
      event(3, { type: "toolCallStarted", callId: "a" }),
      event(4, { type: "toolCallCompleted", callId: "a", status: "succeeded" }),
      event(5, { type: "toolCallCompleted", callId: "b", status: "failed" }),
      event(6, { type: "contextEntriesApplied", entries: [item("result-a", { type: "toolResult", callId: "a", isError: false }, {
        text: "Done", source: { type: "tool", runId: "run-test", turnId: "turn-test", batchId: "batch-1" },
      })] }),
      event(7, { type: "toolBatchStarted", batchId: "batch-2", calls: [{ ...calls[0]!, callId: "c" }] }),
      event(8, { type: "runCompleted" }),
    ];
    const window = new TranscriptWindow();
    window.append(events.slice(4));
    expect(window.state.entries.at(-1)).toMatchObject({ toolCalls: undefined, usageComplete: false });
    window.prepend(events.slice(0, 4));
    expect(window.state.entries.at(-1)).toMatchObject({ toolCalls: 3, usageComplete: true });
    window.append(events);
    expect(window.state.entries.at(-1)).toMatchObject({ toolCalls: 3 });
    window.append([event(9, { type: "runStarted", runId: "other" }), event(10, { type: "runCompleted", runId: "other" })]);
    expect(window.state.entries.at(-1)).toMatchObject({ toolCalls: 0 });
  });

  it("keeps the last call context separate from cumulative usage, including across event pages", () => {
    let state = applyEvents(emptyTranscript(), [
      event(1, { type: "runStarted" }, 1_000),
      event(2, { type: "turnGenerationCompleted", usage: { inputTokens: 9000, cachedInputTokens: 8000, outputTokens: 1000 } }),
    ]);
    state = applyEvents(state, [
      event(3, { type: "turnGenerationCompleted", usage: { inputTokens: 11000, cachedInputTokens: 10000, outputTokens: 2000 } }),
    ]);
    expect(state.entries).toEqual([]);
    state = applyEvents(state, [event(4, { type: "runCompleted" }, 9_200)]);
    expect(state.entries).toMatchObject([{
      kind: "run-summary", status: "completed", contextTokens: 11000, durationMs: 8200,
      usageComplete: true,
      usage: { inputTokens: 20000, cachedInputTokens: 18000, outputTokens: 3000, modelCalls: 2 },
    }]);
  });

  it("retains duration when usage is unreported and never turns missing counts into zero", () => {
    const state = applyEvents(emptyTranscript(), [
      event(1, { type: "runStarted" }, 1_000),
      event(2, { type: "turnGenerationCompleted" }),
      event(3, { type: "turnGenerationCompleted", usage: { inputTokens: 5000, cachedInputTokens: 4000, outputTokens: 200 } }),
      event(4, { type: "runCompleted" }, 2_500),
    ]);
    expect(state.entries).toMatchObject([{
      contextTokens: 5000, durationMs: 1500, usageComplete: true,
      usage: { inputTokens: undefined, cachedInputTokens: undefined, outputTokens: undefined, modelCalls: 2 },
    }]);
  });

  it("clears last-call context if the newest call did not report it, without inventing a cache percentage", () => {
    const state = applyEvents(emptyTranscript(), [
      event(1, { type: "runStarted" }),
      event(2, { type: "turnGenerationCompleted", usage: { inputTokens: 5000, cachedInputTokens: 4000, outputTokens: 200 } }),
      event(3, { type: "turnGenerationCompleted", usage: { outputTokens: 50 } }),
      event(4, { type: "runCompleted" }),
    ]);
    expect(state.entries).toMatchObject([{
      contextTokens: undefined,
      usage: { inputTokens: undefined, cachedInputTokens: undefined, outputTokens: 250, modelCalls: 2 },
    }]);
  });

  it("preserves explicit zero usage and cache hits", () => {
    const state = applyEvents(emptyTranscript(), [
      event(1, { type: "runStarted" }),
      event(2, { type: "turnGenerationCompleted", usage: { inputTokens: 5000, cachedInputTokens: 0, outputTokens: 0 } }),
      event(3, { type: "runCompleted" }),
    ]);
    expect(state.entries).toMatchObject([{
      usage: { inputTokens: 5000, cachedInputTokens: 0, outputTokens: 0, modelCalls: 1 },
    }]);
  });

  it("uses the authoritative run start time when event catch-up missed it", () => {
    let state = reconcileRuns(emptyTranscript(), [{ ...runView("run-test", "running"), startedAtMs: 1_000 }]);
    state = applyEvents(state, [event(3, { type: "runCompleted" }, 3_500)]);
    expect(state.entries).toMatchObject([{ durationMs: 2500, usageComplete: false }]);
  });

  it.each(["runFailed", "runCancelled"] as const)("retains statistics for %s", (type) => {
    const state = applyEvents(emptyTranscript(), [
      event(1, { type: "runStarted" }),
      event(2, { type: "turnGenerationCompleted", usage: { inputTokens: 1200, outputTokens: 70 } }),
      event(3, { type }),
    ]);
    expect(state.entries).toMatchObject([{
      kind: "run-summary", status: type === "runFailed" ? "failed" : "cancelled", durationMs: 2,
      contextTokens: 1200, usage: { inputTokens: 1200, outputTokens: 70, modelCalls: 1 },
    }]);
  });

  it.each([[0, "0"], [750, "750"], [999, "999"], [1000, "1k"], [4200, "4.2k"], [78512, "78.5k"], [733000, "733k"]])(
    "formats %s tokens as %s", (count, expected) => expect(formatTokens(count as number)).toBe(expected),
  );

  it.each([[0, "0ms"], [42, "42ms"], [99, "99ms"], [100, "0.1s"], [340, "0.3s"], [1_250, "1.3s"], [9_999, "10.0s"], [10_000, "10s"], [84_000, "1m 24s"], [3_600_000, "1h"]])(
    "formats %s ms as %s", (ms, expected) => expect(formatDuration(ms as number)).toBe(expected),
  );
});

describe("run attribution and timing", () => {
  it("stamps the run on tool groups and reasoning and measures calls from observed times", () => {
    const state = applyEvents(emptyTranscript(), [
      event(1, { type: "runStarted", runId: "run_9" }, 1_000),
      event(2, {
        type: "contextEntriesApplied",
        entries: [item("thought", { type: "reasoningState" }, {
          preview: "**Plan**", source: { type: "assistantOutput", runId: "run_9", turnId: "turn-1" },
        })],
      }, 1_500),
      event(3, {
        type: "toolBatchStarted", runId: "run_9", batchId: "b1",
        calls: [{ callId: "c1", toolName: "read_file", argumentsRef: "sha256:a" }],
      }, 2_000),
      event(4, { type: "toolCallStarted", runId: "run_9", batchId: "b1", callId: "c1" }, 2_500),
      event(5, { type: "toolCallCompleted", runId: "run_9", batchId: "b1", callId: "c1", status: "succeeded", outputBytes: 412 }, 3_750),
      event(6, { type: "runCompleted", runId: "run_9" }, 4_000),
    ]);
    expect(state.activeRun).toBeNull();
    expect(state.entries).toMatchObject([
      { kind: "reasoning", runId: "run_9" },
      { kind: "tool-group", runId: "run_9", calls: [{
        callId: "c1", startedAtMs: 2_500, completedAtMs: 3_750, durationMs: 1_250, outputBytes: 412,
      }] },
      { kind: "run-summary", runId: "run_9", durationMs: 3_000 },
    ]);
  });

  it("carries the run start onto the active run and leaves a call's duration unknown without its start", () => {
    const state = applyEvents(emptyTranscript(), [
      event(1, { type: "runStarted", runId: "run_9" }, 5_000),
      event(2, { type: "toolCallCompleted", runId: "run_9", batchId: "b1", callId: "c1", status: "succeeded" }, 6_000),
    ]);
    expect(state.activeRun).toMatchObject({ runId: "run_9", startedAtMs: 5_000 });
    expect(state.entries).toMatchObject([{ kind: "tool-group", runId: "run_9", calls: [{ callId: "c1", completedAtMs: 6_000 }] }]);
    const call = (state.entries[0] as Extract<TranscriptEntry, { kind: "tool-group" }>).calls[0]!;
    expect(call.durationMs).toBeUndefined();
    expect(call.startedAtMs).toBeUndefined();
  });

  it("attributes a context-only tool group to the run its items name", () => {
    const state = applyEvents(emptyTranscript(), [event(1, {
      type: "contextEntriesApplied",
      entries: [
        item("tool", { type: "toolCall", callId: "c1", name: "grep" }, { source: { type: "assistantOutput", runId: "run_4", turnId: "t" } }),
        item("result", { type: "toolResult", callId: "c1", isError: false }, { text: "ok", source: { type: "tool", runId: "run_4", turnId: "t", batchId: "b" } }),
      ],
    })]);
    expect(state.entries).toMatchObject([{ kind: "tool-group", runId: "run_4", status: "succeeded" }]);
  });
});
