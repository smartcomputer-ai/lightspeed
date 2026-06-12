import type { SessionView } from "@forge/agent-client";
import { describe, expect, it } from "vitest";
import { extractLatestAssistantText } from "../src/forge.js";

function sessionFixture(): SessionView {
  return {
    activeContext: {
      revision: 3,
      items: [
        { id: "u1", type: "userMessage", text: "question" },
        { id: "a1", type: "assistantMessage", text: "old answer" },
        { id: "a2", type: "assistantMessage", text: "fallback answer" },
      ],
    },
    configRevision: 0,
    createdAtMs: 1,
    id: "session_1",
    runs: [
      {
        id: "run_1",
        input: [{ type: "text", text: "question" }],
        items: [
          { id: "u1", type: "userMessage", text: "question" },
          { id: "a1", type: "assistantMessage", text: "old answer" },
        ],
        status: "completed",
      },
      {
        id: "run_2",
        input: [{ type: "text", text: "follow up" }],
        items: [
          { id: "u2", type: "userMessage", text: "follow up" },
          { id: "a3", type: "assistantMessage", text: "run answer" },
        ],
        status: "completed",
      },
    ],
    status: "idle",
    updatedAtMs: 2,
  };
}

describe("extractLatestAssistantText", () => {
  it("prefers assistant text from the matching run", () => {
    expect(extractLatestAssistantText(sessionFixture(), "run_2")).toBe("run answer");
  });

  it("falls back to active context", () => {
    expect(extractLatestAssistantText(sessionFixture(), "missing")).toBe("fallback answer");
  });
});
