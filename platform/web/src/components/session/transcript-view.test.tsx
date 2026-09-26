import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { SessionEvent } from "@/api";
import { applyEvents, emptyTranscript } from "@/lib/sessions/transcript";
import { ApprovalCards, QueuedRunsBar, TranscriptEntryView } from "./transcript-view";

describe("TranscriptEntryView", () => {
  it("renders native compaction as a marker without exposing or loading encrypted contents", () => {
    const event: SessionEvent = {
      cursor: { seq: 1 }, observedAtMs: 1, joins: {}, sessionId: "session-test",
      kind: {
        type: "contextEntriesApplied", baseRevision: 0, revision: 1,
        entries: [{
          id: "native-compaction", kind: { type: "providerOpaque" },
          content: {
            contentRef: "sha256:compaction", mediaType: "application/json",
            providerKind: "openai.responses.compaction",
          },
          text: '{"encrypted_content":"hidden-encrypted-payload"}',
          preview: "hidden-encrypted-preview",
        }],
      },
    };
    const state = applyEvents(emptyTranscript(), [event]);
    const loadFullText = vi.fn();
    const html = state.entries.map((entry) => renderToString(createElement(TranscriptEntryView, {
      entry, loadFullText,
    }))).join("");
    expect(html).toContain("context compacted");
    expect(html).not.toContain("hidden-encrypted");
    expect(html).not.toContain("encrypted_content");
    expect(html).not.toContain("sha256:compaction");
    expect(html).not.toContain("Expand full entry");
    expect(loadFullText).not.toHaveBeenCalled();
  });

  it.each(["user", "assistant"] as const)("retains full %s message text without fetching it", (role) => {
    const text = "Complete message 🦀. ".repeat(700) + "The final sentence.";
    const loadFullText = vi.fn();
    const html = renderToString(createElement(TranscriptEntryView, {
      entry: { kind: "message", key: "message", role, text },
      loadFullText,
    }));
    expect(html).toContain(text);
    expect(html).not.toContain("Expand full entry");
    expect(loadFullText).not.toHaveBeenCalled();
  });
});


it("preserves pending approval details without offering decisions to readers", () => {
  const html = renderToString(createElement(ApprovalCards, {
    approvals: [{ approvalId: "approval", requestedAtMs: 0, subject: {
      kind: "mcpToolCall", argumentsPreview: '{"query":"report"}', argumentsRef: "blob",
      serverId: "server", serverLabel: "Finance", toolName: "read_report",
    } }], deciding: null, error: null,
  }));
  expect(html).toContain("read_report");
  expect(html).toContain("Waiting for the session controller to decide.");
  expect(html).not.toContain("<button");
});
it("preserves the queued message list without cancellation controls for readers", () => {
  const html = renderToString(createElement(QueuedRunsBar, {
    items: [{ key: "queued", runId: "run", text: "Queued work", cancelling: false }],
  }));
  expect(html).toContain("Queued work");
  expect(html).not.toContain("Cancel queued message");
});
