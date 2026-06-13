import type { SessionView } from "@lightspeed/agent-client";
import { describe, expect, it } from "vitest";
import { extractAssistantText } from "../src/lightspeed.js";

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
          { id: "a3", type: "assistantMessage", text: "first part" },
          { id: "a4", type: "assistantMessage", text: "second part" },
        ],
        status: "completed",
      },
    ],
    status: "idle",
    updatedAtMs: 2,
  };
}

describe("extractAssistantText", () => {
  it("joins every assistant message from the matching run", () => {
    expect(extractAssistantText(sessionFixture(), "run_2")).toBe("first part\n\nsecond part");
  });

  it("falls back to the latest active-context assistant text", () => {
    expect(extractAssistantText(sessionFixture(), "missing")).toBe("fallback answer");
  });
});
