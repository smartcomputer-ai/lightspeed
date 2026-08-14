import { ApplicationFailure } from "@temporalio/common";
import { describe, expect, it, vi } from "vitest";
import type { ChannelDeliveryCommandV1 } from "../src/contracts/delivery.js";
import {
  createWhatsAppDeliveryActivities,
  type WhatsAppDeliveryApi,
} from "../src/providers/whatsapp/delivery.js";

function command(
  operation: ChannelDeliveryCommandV1["operation"],
): ChannelDeliveryCommandV1 {
  return {
    version: 1,
    invocationId: `wti:sha256:${"a".repeat(64)}`,
    idempotencyKey: `wti:sha256:${"a".repeat(64)}`,
    route: { provider: "whatsapp", accountId: "primary", chatId: "family@g.us" },
    operation,
  };
}

describe("WhatsApp delivery activity", () => {
  it("renders and returns every provider message id", async () => {
    const sendMessage = vi
      .fn<WhatsAppDeliveryApi["sendMessage"]>()
      .mockResolvedValueOnce({ key: { id: "wamid-1" } })
      .mockResolvedValueOnce({ key: { id: "wamid-2" } });
    const activities = createWhatsAppDeliveryActivities({
      accountId: "primary",
      api: { sendMessage },
    });

    await expect(
      activities.deliverChannelMessage(
        command({ type: "send", text: `**bold** ${"x".repeat(3_600)}` }),
      ),
    ).resolves.toEqual({
      version: 1,
      provider: "whatsapp",
      messageIds: ["wamid-1", "wamid-2"],
    });
    expect(sendMessage).toHaveBeenNthCalledWith(
      1,
      "family@g.us",
      expect.objectContaining({ text: expect.stringContaining("*bold*") }),
    );
  });

  it("builds native edit and reaction keys", async () => {
    const sendMessage = vi.fn<WhatsAppDeliveryApi["sendMessage"]>(async () => ({}));
    const activities = createWhatsAppDeliveryActivities({
      accountId: "primary",
      api: { sendMessage },
    });
    await activities.deliverChannelMessage(
      command({ type: "edit", messageId: "wamid-1", text: "new" }),
    );
    await activities.deliverChannelMessage(
      command({ type: "react", messageId: "wamid-2", emoji: "👍" }),
    );
    expect(sendMessage).toHaveBeenNthCalledWith(1, "family@g.us", {
      text: "new",
      edit: { remoteJid: "family@g.us", id: "wamid-1", fromMe: true },
    });
    expect(sendMessage).toHaveBeenNthCalledWith(2, "family@g.us", {
      react: {
        text: "👍",
        key: { remoteJid: "family@g.us", id: "wamid-2", fromMe: false },
      },
    });
  });

  it("constructs a native group quote from durable reply context", async () => {
    const sendMessage = vi.fn<WhatsAppDeliveryApi["sendMessage"]>(async () => ({
      key: { id: "wamid-reply" },
    }));
    const activities = createWhatsAppDeliveryActivities({
      accountId: "primary",
      api: { sendMessage },
    });
    await activities.deliverChannelMessage(
      command({
        type: "send",
        text: "answer",
        replyTo: "wamid-question",
        replyContext: {
          senderId: "41791111111@s.whatsapp.net",
          text: "question",
        },
      }),
    );
    expect(sendMessage).toHaveBeenCalledWith(
      "family@g.us",
      { text: "answer" },
      {
        quoted: {
          key: {
            remoteJid: "family@g.us",
            id: "wamid-question",
            fromMe: false,
            participant: "41791111111@s.whatsapp.net",
          },
          message: { conversation: "question" },
        },
      },
    );
  });

  it("classifies routing errors as non-retryable", async () => {
    const sendMessage = vi.fn<WhatsAppDeliveryApi["sendMessage"]>();
    const activities = createWhatsAppDeliveryActivities({
      accountId: "other",
      api: { sendMessage },
    });
    const error = await activities
      .deliverChannelMessage(command({ type: "send", text: "hello" }))
      .catch((caught: unknown) => caught);
    expect(error).toBeInstanceOf(ApplicationFailure);
    expect((error as ApplicationFailure).nonRetryable).toBe(true);
    expect(sendMessage).not.toHaveBeenCalled();
  });
});
