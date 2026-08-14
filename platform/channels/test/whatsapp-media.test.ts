import { describe, expect, it, vi } from "vitest";
import { normalizeWhatsAppInbound } from "../src/providers/whatsapp/ingress.js";
import {
  createWhatsAppMediaActivities,
  describeWhatsAppMedia,
  parseWhatsAppMediaLocatorKey,
} from "../src/providers/whatsapp/media.js";

const universeId = "6f3fac85-1ec8-4c27-9c97-f403355d5e6f";
const accountId = "primary";
const locatorKey = Buffer.alloc(32, 7);
const mediaKey = Buffer.from("message-specific-media-key");

describe("WhatsApp media", () => {
  it("places only an encrypted download locator in the durable inbound envelope", () => {
    const media = describeWhatsAppMedia(accountId, locatorKey, {
      messageId: "wamid-1",
      mediaType: "audio",
      reportedMime: "audio/opus",
      byteSize: 123,
      mediaKey,
      directPath: "/v/t62/example.enc",
      voiceNote: true,
    });
    expect(media).toMatchObject({
      provider: "whatsapp",
      kind: "audio",
      mime: "audio/ogg",
      name: "voice.ogg",
      byteSize: 123,
    });
    expect(media?.fileId).toMatch(/^wam1\./);
    expect(media?.fileId).not.toContain(mediaKey.toString("base64"));

    const inbound = normalizeWhatsAppInbound(
      { accountId, ownJids: new Set(["41790000000@s.whatsapp.net"]) },
      {
        messageId: "wamid-1",
        remoteJid: "41791111111@s.whatsapp.net",
        timestampMs: 1_700_000_000_000,
        text: "",
        ...(media === null ? {} : { media: [media] }),
      },
    );
    expect(inbound).toMatchObject({ text: "(sent a voice note)", media: [{ kind: "audio" }] });
    expect(JSON.stringify(inbound)).not.toContain(mediaKey.toString("base64"));
  });

  it("decrypts inside the account worker, streams with a bound, and uploads to CAS", async () => {
    const media = describeWhatsAppMedia(accountId, locatorKey, {
      messageId: "wamid-2",
      mediaType: "document",
      reportedMime: "text/plain",
      fileName: "notes.txt",
      byteSize: 11,
      mediaKey,
      url: "https://mmg.whatsapp.net/example",
    });
    if (media === null) throw new Error("test media was rejected");
    const download = vi.fn(async (locator: { mediaKey: Uint8Array }, type: string) => {
      expect(Buffer.from(locator.mediaKey)).toEqual(mediaKey);
      expect(type).toBe("document");
      return (async function* () {
        yield Buffer.from("hello ");
        yield Buffer.from("media");
      })();
    });
    let rpc: Record<string, unknown> | undefined;
    const fetch = vi.fn<typeof globalThis.fetch>(async (_input, init) => {
      rpc = JSON.parse(String(init?.body)) as Record<string, unknown>;
      return Response.json({
        id: rpc.id,
        result: { result: { blobs: [{ blobRef: `sha256:${"c".repeat(64)}` }] } },
      });
    });
    const activities = createWhatsAppMediaActivities({
      accountId,
      locatorKey,
      lightspeedEndpoint: "http://lightspeed.test/rpc",
      download,
      fetch,
    });
    await expect(
      activities.prepareChannelMedia({
        universeId,
        route: { provider: "whatsapp", accountId, chatId: "41791111111@s.whatsapp.net" },
        media,
      }),
    ).resolves.toEqual({
      item: {
        type: "media",
        blobRef: `sha256:${"c".repeat(64)}`,
        kind: "document",
        mime: "text/plain",
        name: "notes.txt",
      },
    });
    expect(rpc).toMatchObject({
      method: "blobs/put",
      params: { blobs: [{ bytesBase64: Buffer.from("hello media").toString("base64") }] },
    });
  });

  it("requires a deployment key of exactly 32 bytes", () => {
    expect(parseWhatsAppMediaLocatorKey(locatorKey.toString("base64"))).toEqual(locatorKey);
    expect(() => parseWhatsAppMediaLocatorKey(Buffer.alloc(31).toString("base64"))).toThrow(
      /32 bytes/,
    );
  });
});
