import { describe, expect, it } from "vitest";
import { controlledSessionBotId, controlledSessionOwnerLabel } from "./lifecycle-controller";

describe("lifecycle controller ownership", () => {
  it("resolves current bot controllers and prefers their explicit metadata", () => {
    const declaration = {
      version: 1,
      lifecycleController: {
        workflowKind: "BotControllerWorkflow",
        workflowId: "6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f/bot-triage",
      },
    };
    expect(controlledSessionBotId(declaration, undefined)).toBe("triage");
    expect(controlledSessionBotId(declaration, { bot: "implementer" })).toBe("implementer");
    expect(controlledSessionOwnerLabel(declaration)).toBe("a bot");
  });

  it("does not infer ownership from non-bot lifecycle controllers", () => {
    const declaration = {
      version: 1,
      lifecycleController: {
        workflowKind: "ChannelConversationWorkflow",
        workflowId: "universe/chat-telegram-example",
      },
    };
    expect(controlledSessionBotId(declaration, { bot: "spoofed" })).toBeNull();
  });
});
