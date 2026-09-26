import { describe, expect, it, vi } from "vitest";
import { CoreClient } from "../src/core/client.js";
import { normalizeWhatsAppInbound } from "../src/providers/whatsapp/ingress.js";
import {
  createWhatsAppMediaActivities,
  describeWhatsAppMedia,
  parseWhatsAppMediaLocatorKey,
  whatsAppMediaScope,
} from "../src/providers/whatsapp/media.js";
import { UNIVERSE_A, UNIVERSE_B, fakeRpc } from "./fixtures.js";

const accountId = "primary";
const scope = whatsAppMediaScope(UNIVERSE_A, accountId);
const locatorKey = Buffer.alloc(32, 7);
const mediaKey = Buffer.from("message-specific-media-key");

describe("WhatsApp media", () => {
  it("places only an encrypted download locator in the inbound envelope", () => {
    const media = describeWhatsAppMedia(scope, locatorKey, {
      messageId: "wamid-1",
      mediaType: "audio",
      reportedMime: "audio/opus",
      byteSize: 123,
      mediaKey,
      directPath: "/v/t62/example.enc",
      voiceNote: true,
    });
    expect(media).toMatchObject({ kind: "audio", mime: "audio/ogg", name: "voice.ogg", byteSize: 123 });
    expect(media?.fileId).toMatch(/^wam1\./);
    expect(media?.fileId).not.toContain(mediaKey.toString("base64"));

    const inbound = normalizeWhatsAppInbound(
      { ownJids: new Set(["41790000000@s.whatsapp.net"]) },
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
    const media = describeWhatsAppMedia(scope, locatorKey, {
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
    const rpc = fakeRpc(() => ({ blobs: [{ blobRef: `sha256:${"c".repeat(64)}`, bytes: 11 }] }));
    const core = new CoreClient({ apiKey: "lsk_test_connector", endpoint: "http://lightspeed.test/rpc", fetch: rpc.fetch });
    const activities = createWhatsAppMediaActivities({
      universeId: UNIVERSE_A,
      accountId,
      locatorKey,
      core: core.forUniverse(UNIVERSE_A),
      download,
    });
    await expect(
      activities.prepareChannelMedia({
        universeId: UNIVERSE_A,
        route: { provider: "whatsapp", accountId, chatId: "41791111111@s.whatsapp.net" },
        media,
      }),
    ).resolves.toEqual({
      item: { blobRef: `sha256:${"c".repeat(64)}`, kind: "document", mime: "text/plain", name: "notes.txt" },
    });
    expect(rpc.calls[0]).toMatchObject({
      method: "blobs/put",
      params: { blobs: [{ bytesBase64: Buffer.from("hello media").toString("base64") }] },
    });
    expect(rpc.calls[0]?.headers.get("x-lightspeed-universe")).toBe(UNIVERSE_A);
  });

  it("binds locators to the account inside its universe", async () => {
    const media = describeWhatsAppMedia(scope, locatorKey, {
      messageId: "wamid-3",
      mediaType: "image",
      mediaKey,
      url: "https://mmg.whatsapp.net/example",
    });
    if (media === null) throw new Error("test media was rejected");
    const download = vi.fn();
    const otherUniverse = createWhatsAppMediaActivities({
      universeId: UNIVERSE_B,
      accountId,
      locatorKey,
      core: new CoreClient({ apiKey: "lsk_test_connector", endpoint: "http://lightspeed.test/rpc" }).forUniverse(UNIVERSE_B),
      download,
    });
    await expect(
      otherUniverse.prepareChannelMedia({
        universeId: UNIVERSE_B,
        route: { provider: "whatsapp", accountId, chatId: "x@s.whatsapp.net" },
        media,
      }),
    ).rejects.toMatchObject({ nonRetryable: true, type: "ChannelMediaRejected" });
    expect(download).not.toHaveBeenCalled();
  });

  it("requires a deployment key of exactly 32 bytes", () => {
    expect(parseWhatsAppMediaLocatorKey(locatorKey.toString("base64"))).toEqual(locatorKey);
    expect(() => parseWhatsAppMediaLocatorKey(Buffer.alloc(31).toString("base64"))).toThrow(/32 bytes/);
  });
});
