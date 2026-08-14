import { ApplicationFailure } from "@temporalio/common";
import { describe, expect, it, vi } from "vitest";
import type { ChannelDeliveryCommandV1 } from "../src/contracts/delivery.js";
import {
  createTelegramDeliveryActivities,
  type TelegramDeliveryApi,
} from "../src/providers/telegram/delivery.js";

function command(
  operation: ChannelDeliveryCommandV1["operation"],
): ChannelDeliveryCommandV1 {
  return {
    version: 1,
    invocationId: `wti:sha256:${"a".repeat(64)}`,
    idempotencyKey: `wti:sha256:${"a".repeat(64)}`,
    route: {
      provider: "telegram",
      accountId: "primary",
      chatId: "-100123",
      threadId: "7",
    },
    operation,
  };
}

function fakeApi(): TelegramDeliveryApi {
  return {
    sendMessage: vi.fn(async () => ({ message_id: 42 })),
    editMessageText: vi.fn(async () => true),
    setMessageReaction: vi.fn(async () => true),
  };
}

describe("Telegram delivery activity", () => {
  it("renders, chunks, threads, and replies only on the first chunk", async () => {
    const api = fakeApi();
    vi.mocked(api.sendMessage)
      .mockResolvedValueOnce({ message_id: 42 })
      .mockResolvedValueOnce({ message_id: 43 });
    const activities = createTelegramDeliveryActivities({ accountId: "primary", api });

    const result = await activities.deliverChannelMessage(
      command({ type: "send", text: `**bold** ${"x".repeat(4_100)}`, replyTo: "41" }),
    );

    expect(result).toEqual({ version: 1, provider: "telegram", messageIds: ["42", "43"] });
    expect(api.sendMessage).toHaveBeenCalledTimes(2);
    expect(api.sendMessage).toHaveBeenNthCalledWith(
      1,
      "-100123",
      expect.stringContaining("<b>bold</b>"),
      expect.objectContaining({
        parse_mode: "HTML",
        message_thread_id: 7,
        reply_parameters: { message_id: 41 },
      }),
    );
    expect(vi.mocked(api.sendMessage).mock.calls[1]?.[2]).not.toHaveProperty("reply_parameters");
  });

  it("falls back to plain text when Telegram rejects generated HTML", async () => {
    const api = fakeApi();
    vi.mocked(api.sendMessage)
      .mockRejectedValueOnce(new Error("Bad Request: can't parse entities"))
      .mockResolvedValueOnce({ message_id: 42 });
    const activities = createTelegramDeliveryActivities({ accountId: "primary", api });

    await activities.deliverChannelMessage(command({ type: "send", text: "**bold**" }));

    expect(api.sendMessage).toHaveBeenNthCalledWith(2, "-100123", "**bold**", {
      message_thread_id: 7,
    });
  });

  it("supports edit and reaction receipts", async () => {
    const api = fakeApi();
    const activities = createTelegramDeliveryActivities({ accountId: "primary", api });
    await expect(
      activities.deliverChannelMessage(command({ type: "edit", messageId: "42", text: "new" })),
    ).resolves.toMatchObject({ messageIds: ["42"] });
    await expect(
      activities.deliverChannelMessage(command({ type: "react", messageId: "42", emoji: "👍" })),
    ).resolves.toMatchObject({ messageIds: ["42"] });
    expect(api.editMessageText).toHaveBeenCalledWith("-100123", 42, "new", {
      parse_mode: "HTML",
    });
    expect(api.setMessageReaction).toHaveBeenCalledWith("-100123", 42, [
      { type: "emoji", emoji: "👍" },
    ]);
  });

  it("marks invalid routing as a non-retryable Temporal failure", async () => {
    const api = fakeApi();
    const activities = createTelegramDeliveryActivities({ accountId: "other", api });
    const error = await activities
      .deliverChannelMessage(command({ type: "send", text: "hello" }))
      .catch((caught: unknown) => caught);
    expect(error).toBeInstanceOf(ApplicationFailure);
    expect((error as ApplicationFailure).nonRetryable).toBe(true);
    expect(api.sendMessage).not.toHaveBeenCalled();
  });
});
