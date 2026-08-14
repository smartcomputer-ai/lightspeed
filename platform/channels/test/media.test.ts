import { ApplicationFailure } from "@temporalio/common";
import { describe, expect, it, vi } from "vitest";
import { parseNormalizedInboundV1 } from "../src/contracts/channel.js";
import {
  MAX_AUDIO_BYTES,
  MAX_IMAGE_BYTES,
  audioMime,
  documentMime,
  mediaByteLimit,
} from "../src/media/validation.js";
import { normalizeTelegramInbound } from "../src/providers/telegram/ingress.js";
import { createTelegramMediaActivities } from "../src/providers/telegram/media.js";

describe("channel media admission", () => {
  it("normalizes provider MIME aliases and enforces kind-specific limits", () => {
    expect(documentMime("notes.MD", "application/octet-stream")).toBe("text/markdown");
    expect(documentMime("malware.exe", "application/octet-stream")).toBeNull();
    expect(audioMime("voice.opus", "application/octet-stream")).toBe("audio/ogg");
    expect(audioMime(undefined, "audio/x-m4a; codecs=aac")).toBe("audio/mp4");
    expect(mediaByteLimit("image", "image/jpeg")).toBe(MAX_IMAGE_BYTES);
    expect(mediaByteLimit("audio", "audio/ogg")).toBe(MAX_AUDIO_BYTES);
    expect(mediaByteLimit("document", "text/plain")).toBe(1024 * 1024);
  });

  it("accepts a Telegram media-only message and selects the largest admissible photo", () => {
    const inbound = normalizeTelegramInbound(
      { accountId: "primary", botId: 99 },
      {
        messageId: 42,
        chatId: 123,
        chatType: "private",
        senderId: 7,
        senderFirstName: "Ada",
        timestampMs: 1_700_000_000_000,
        photos: [
          { fileId: "small", width: 100, height: 100, fileSize: 1_000 },
          { fileId: "large", width: 1_000, height: 1_000, fileSize: 2_000 },
          { fileId: "too-large", width: 2_000, height: 2_000, fileSize: MAX_IMAGE_BYTES + 1 },
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
        },
      ],
    });
    expect(() => parseNormalizedInboundV1(inbound)).not.toThrow();
  });

  it("rejects media whose provider does not match the route", () => {
    expect(() =>
      parseNormalizedInboundV1({
        version: 1,
        messageId: "1",
        route: { provider: "telegram", accountId: "primary", chatId: "123" },
        senderId: "7",
        senderName: "Ada",
        timestampMs: 1,
        text: "file",
        media: [
          {
            version: 1,
            provider: "whatsapp",
            fileId: "file-1",
            kind: "document",
            mime: "text/plain",
          },
        ],
        isDirect: true,
        mentionedBot: false,
        isReplyToBot: false,
      }),
    ).toThrow(/must match/);
  });
});

describe("Telegram media activity", () => {
  it("downloads on the account worker and uploads bytes directly to universe-scoped CAS", async () => {
    const calls: Array<{ url: string; body?: Record<string, unknown>; headers?: Headers }> = [];
    const request = vi.fn<typeof fetch>(async (input, init) => {
      const url = String(input);
      if (url.startsWith("https://api.telegram.org/file/")) {
        calls.push({ url });
        return new Response(new TextEncoder().encode("hello media"), {
          headers: { "content-length": "11" },
        });
      }
      const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
      calls.push({ url, body, headers: new Headers(init?.headers) });
      return Response.json({
        id: body.id,
        result: { result: { blobs: [{ blobRef: `sha256:${"a".repeat(64)}` }] } },
      });
    });
    const activities = createTelegramMediaActivities({
      accountId: "primary",
      botToken: "secret-token",
      lightspeedEndpoint: "http://lightspeed.test/rpc",
      api: { getFile: async () => ({ file_path: "documents/note.txt", file_size: 11 }) },
      fetch: request,
    });

    await expect(
      activities.prepareChannelMedia({
        universeId: "6f3fac85-1ec8-4c27-9c97-f403355d5e6f",
        route: { provider: "telegram", accountId: "primary", chatId: "123" },
        media: {
          version: 1,
          provider: "telegram",
          fileId: "telegram-file-1",
          kind: "document",
          mime: "text/plain",
          name: "note.txt",
          byteSize: 11,
        },
      }),
    ).resolves.toEqual({
      item: {
        type: "media",
        blobRef: `sha256:${"a".repeat(64)}`,
        kind: "document",
        mime: "text/plain",
        name: "note.txt",
      },
    });
    expect(calls[0]?.url).toContain("/botsecret-token/documents/note.txt");
    expect(calls[1]?.body).toMatchObject({
      method: "blobs/put",
      params: { blobs: [{ bytesBase64: Buffer.from("hello media").toString("base64") }] },
    });
    expect(calls[1]?.headers?.get("x-lightspeed-universe")).toBe(
      "6f3fac85-1ec8-4c27-9c97-f403355d5e6f",
    );
  });

  it("rejects the wrong account before touching Telegram", async () => {
    const getFile = vi.fn(async () => ({ file_path: "photo.jpg" }));
    const activities = createTelegramMediaActivities({
      accountId: "primary",
      botToken: "secret-token",
      lightspeedEndpoint: "http://lightspeed.test/rpc",
      api: { getFile },
    });
    const result = activities.prepareChannelMedia({
      universeId: "universe",
      route: { provider: "telegram", accountId: "secondary", chatId: "123" },
      media: {
        version: 1,
        provider: "telegram",
        fileId: "file",
        kind: "image",
        mime: "image/jpeg",
      },
    });
    await expect(result).rejects.toBeInstanceOf(ApplicationFailure);
    await expect(result).rejects.toMatchObject({ nonRetryable: true });
    expect(getFile).not.toHaveBeenCalled();
  });
});
