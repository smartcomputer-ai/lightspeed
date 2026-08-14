import { describe, expect, it } from "vitest";
import { normalizeTelegramInbound } from "../src/providers/telegram/ingress.js";
import { normalizeWhatsAppInbound } from "../src/providers/whatsapp/ingress.js";

describe("provider ingress normalization", () => {
  it("normalizes Telegram topics, mentions, and replies", () => {
    const text = "@Lightspeed hello";
    expect(
      normalizeTelegramInbound(
        { accountId: "primary", botId: 99, botUsername: "Lightspeed" },
        {
          messageId: 42,
          chatId: -100123,
          chatType: "supergroup",
          threadId: 7,
          senderId: 12,
          timestampMs: 1_700_000_000_000,
          senderFirstName: "Lukas",
          senderLastName: "Müller",
          text,
          entities: [{ type: "mention", offset: 0, length: 11 }],
          replyToSenderId: 99,
        },
      ),
    ).toMatchObject({
      messageId: "42",
      route: { provider: "telegram", chatId: "-100123", threadId: "7" },
      senderName: "Lukas Müller",
      mentionedBot: true,
      isReplyToBot: true,
      isDirect: false,
    });
  });

  it("drops empty and self-authored Telegram messages", () => {
    const base = {
      messageId: 1,
      chatId: 1,
      chatType: "private" as const,
      senderId: 99,
      timestampMs: 1_700_000_000_000,
      text: "hello",
    };
    expect(normalizeTelegramInbound({ accountId: "primary", botId: 99 }, base)).toBeNull();
    expect(
      normalizeTelegramInbound(
        { accountId: "primary", botId: 99 },
        { ...base, senderId: 12, text: "  " },
      ),
    ).toBeNull();
  });

  it("normalizes Telegram photos and media-only messages without bytes", () => {
    const inbound = normalizeTelegramInbound(
      { accountId: "primary", botId: 99 },
      {
        messageId: 2,
        chatId: 1,
        chatType: "private",
        senderId: 12,
        timestampMs: 1_700_000_000_000,
        photos: [
          { fileId: "small", width: 90, height: 90, fileSize: 100 },
          { fileId: "large", width: 1280, height: 720, fileSize: 1_000 },
        ],
      },
    );
    expect(inbound).toMatchObject({
      text: "(sent an image)",
      media: [
        {
          version: 1,
          provider: "telegram",
          fileId: "large",
          kind: "image",
          mime: "image/jpeg",
          byteSize: 1_000,
        },
      ],
    });
    expect(JSON.stringify(inbound)).not.toContain("bytesBase64");
  });

  it("normalizes WhatsApp group mentions across device-qualified JIDs", () => {
    expect(
      normalizeWhatsAppInbound(
        { accountId: "primary", ownJids: new Set(["41790000000:4@s.whatsapp.net"]) },
        {
          messageId: "wamid-1",
          remoteJid: "family@g.us",
          participantJid: "41791111111@s.whatsapp.net",
          pushName: "Lukas",
          timestampMs: 1_700_000_000_000,
          text: "hello",
          mentionedJids: ["41790000000@s.whatsapp.net"],
          quotedParticipantJid: "41790000000:7@s.whatsapp.net",
        },
      ),
    ).toMatchObject({
      route: { provider: "whatsapp", chatId: "family@g.us" },
      senderId: "41791111111@s.whatsapp.net",
      text: "hello",
      mentionedBot: true,
      isReplyToBot: true,
      isDirect: false,
    });
  });
});
