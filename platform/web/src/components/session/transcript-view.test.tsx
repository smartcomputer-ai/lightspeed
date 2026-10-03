import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { SessionEvent } from "@/api";
import { applyEvents, emptyTranscript } from "@/lib/sessions/transcript";
import { ApprovalCards, QueuedRunsBar, TranscriptEntryView } from "./transcript-view";

describe("TranscriptEntryView", () => {
  it.each(["openai.responses.compaction", "anthropic.messages.compaction"])("renders %s as a marker without exposing or loading its contents", (providerKind) => {
    const event: SessionEvent = {
      cursor: { seq: 1 }, observedAtMs: 1, joins: {}, sessionId: "session-test",
      kind: {
        type: "contextEntriesApplied", baseRevision: 0, revision: 1,
        entries: [{
          id: "native-compaction", kind: { type: "providerOpaque" },
          content: {
            contentRef: "sha256:compaction", mediaType: "application/json",
            providerKind,
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

  it("renders a standalone replacement as one status marker and a failure as an error", () => {
    const loadFullText = vi.fn();
    const state = applyEvents(emptyTranscript(), [{
      cursor: { seq: 1 }, observedAtMs: 1, joins: {}, sessionId: "session-test",
      kind: { type: "contextEntriesApplied", baseRevision: 0, revision: 1,
        entries: [{ id: "replacement", kind: { type: "message", role: "user" },
          content: { contentRef: "sha256:summary" }, text: "Internal replacement summary",
          source: { type: "runtime", label: "standalone_compaction_prefix" },
        }],
      },
    }, {
      cursor: { seq: 2 }, observedAtMs: 2, joins: {}, sessionId: "session-test",
      kind: { type: "contextCompactionFinished", baseRevision: 1, revision: 2, status: "succeeded" },
    }, {
      cursor: { seq: 3 }, observedAtMs: 3, joins: {}, sessionId: "session-test",
      kind: { type: "contextCompactionFinished", baseRevision: 2, revision: 3, status: "failed" },
    }]);
    const html = state.entries.map((entry) => renderToString(createElement(TranscriptEntryView, { entry, loadFullText }))).join("");
    expect(html.match(/context compacted/g)).toHaveLength(1);
    expect(html).toContain("context compaction failed");
    expect(html).toContain("text-destructive");
    expect(html).not.toContain("Internal replacement summary");
    expect(html).not.toContain("sha256:summary");
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
