import { describe, expect, it } from "vitest";
import {
  MAX_AUDIO_BYTES,
  MAX_IMAGE_BYTES,
  MAX_TEXT_DOCUMENT_BYTES,
  audioMime,
  documentMime,
  imageMime,
  mediaByteLimit,
} from "../src/media/validation.js";
import { parseNormalizedInboundV1 } from "../src/contracts/channel.js";

describe("channel media validation", () => {
  it("normalizes supported document and audio aliases", () => {
    expect(documentMime("notes.MD", "application/octet-stream")).toBe("text/markdown");
    expect(documentMime(undefined, "application/pdf; charset=binary")).toBe("application/pdf");
    expect(audioMime("voice.opus", "application/octet-stream")).toBe("audio/ogg");
    expect(audioMime(undefined, "audio/x-wav")).toBe("audio/wav");
    expect(imageMime("image/svg+xml")).toBeNull();
  });

  it("uses media-specific byte limits", () => {
    expect(mediaByteLimit("image", "image/jpeg")).toBe(MAX_IMAGE_BYTES);
    expect(mediaByteLimit("audio", "audio/ogg")).toBe(MAX_AUDIO_BYTES);
    expect(mediaByteLimit("document", "text/plain")).toBe(MAX_TEXT_DOCUMENT_BYTES);
  });

  it("validates that durable media locators match the route", () => {
    const base = {
      version: 1 as const,
      messageId: "42",
      route: { provider: "telegram" as const, accountId: "primary", chatId: "123" },
      senderId: "7",
      senderName: "Lukas",
      timestampMs: 1_700_000_000_000,
      text: "",
      isDirect: true,
      mentionedBot: false,
      isReplyToBot: false,
    };
    expect(
      parseNormalizedInboundV1({
        ...base,
        media: [
          {
            version: 1,
            provider: "telegram",
            fileId: "file-1",
            kind: "image",
            mime: "image/jpeg",
          },
        ],
      }),
    ).toMatchObject({ media: [{ fileId: "file-1" }] });
    expect(() =>
      parseNormalizedInboundV1({
        ...base,
        media: [
          {
            version: 1,
            provider: "whatsapp",
            fileId: "file-1",
            kind: "image",
            mime: "image/jpeg",
          },
        ],
      }),
    ).toThrow(/must match/);
  });
});
