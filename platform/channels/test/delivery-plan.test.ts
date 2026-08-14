import { describe, expect, it } from "vitest";
import { planDeliveryCommands } from "../src/workflows/delivery-plan.js";

const route = { provider: "telegram" as const, accountId: "primary", chatId: "123" };

describe("workflow delivery planning", () => {
  it("records long sends as independently retryable activity commands", () => {
    const commands = planDeliveryCommands("invocation", route, {
      type: "send",
      text: `first ${"x".repeat(3_600)}`,
      replyTo: "41",
      replyContext: { senderId: "7", text: "question" },
    });
    expect(commands).toHaveLength(2);
    expect(commands.map((command) => command.idempotencyKey)).toEqual([
      "invocation:chunk:1/2",
      "invocation:chunk:2/2",
    ]);
    expect(commands[0]?.operation).toMatchObject({ type: "send", replyTo: "41" });
    expect(commands[0]?.operation).toMatchObject({
      replyContext: { senderId: "7", text: "question" },
    });
    expect(commands[1]?.operation).not.toHaveProperty("replyTo");
    expect(commands[1]?.operation).not.toHaveProperty("replyContext");
  });

  it("keeps a single send, edit, or reaction on the invocation id", () => {
    expect(
      planDeliveryCommands("invocation", route, { type: "send", text: "short" })[0]
        ?.idempotencyKey,
    ).toBe("invocation");
    expect(
      planDeliveryCommands("invocation", route, {
        type: "edit",
        messageId: "42",
        text: "fixed",
      }),
    ).toHaveLength(1);
  });
});
