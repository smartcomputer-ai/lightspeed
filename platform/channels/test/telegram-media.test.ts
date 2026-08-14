import { describe, expect, it, vi } from "vitest";
import { createTelegramMediaActivities } from "../src/providers/telegram/media.js";

const universeId = "6f3fac85-1ec8-4c27-9c97-f403355d5e6f";
const route = { provider: "telegram" as const, accountId: "primary", chatId: "123" };

describe("Telegram media activities", () => {
  it("downloads into Lightspeed CAS and returns only a media reference", async () => {
    const bytes = Buffer.from("image bytes");
    const requests: Array<{ url: string; body?: Record<string, unknown>; headers: Headers }> = [];
    const fetch = vi.fn<typeof globalThis.fetch>(async (input, init) => {
      const url = String(input);
      if (url.startsWith("https://api.telegram.org/")) {
        requests.push({ url, headers: new Headers(init?.headers) });
        return new Response(bytes, { headers: { "content-length": String(bytes.byteLength) } });
      }
      const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
      requests.push({ url, body, headers: new Headers(init?.headers) });
      return Response.json({
        id: body.id,
        result: { result: { blobs: [{ blobRef: `sha256:${"a".repeat(64)}`, bytes: bytes.byteLength }] } },
      });
    });
    const activities = createTelegramMediaActivities({
      accountId: "primary",
      botToken: "secret-token",
      lightspeedEndpoint: "http://lightspeed.test/rpc",
      api: { getFile: vi.fn(async () => ({ file_path: "photos/photo 1.jpg", file_size: bytes.byteLength })) },
      fetch,
    });

    await expect(
      activities.prepareChannelMedia({
        universeId,
        route,
        media: {
          version: 1,
          provider: "telegram",
          fileId: "tg-file-1",
          kind: "image",
          mime: "image/jpeg",
          name: "photo.jpg",
          byteSize: bytes.byteLength,
        },
      }),
    ).resolves.toEqual({
      item: {
        type: "media",
        blobRef: `sha256:${"a".repeat(64)}`,
        kind: "image",
        mime: "image/jpeg",
        name: "photo.jpg",
      },
    });
    expect(requests[0]?.url).toBe(
      "https://api.telegram.org/file/botsecret-token/photos/photo%201.jpg",
    );
    expect(requests[1]?.headers.get("x-lightspeed-universe")).toBe(universeId);
    expect(requests[1]?.body).toMatchObject({
      method: "blobs/put",
      params: { blobs: [{ bytesBase64: bytes.toString("base64") }] },
    });
  });

  it("rejects wrong account affinity without contacting Telegram", async () => {
    const getFile = vi.fn(async () => ({ file_path: "photo.jpg" }));
    const activities = createTelegramMediaActivities({
      accountId: "primary",
      botToken: "secret-token",
      lightspeedEndpoint: "http://lightspeed.test/rpc",
      api: { getFile },
    });
    await expect(
      activities.prepareChannelMedia({
        universeId,
        route: { ...route, accountId: "other" },
        media: {
          version: 1,
          provider: "telegram",
          fileId: "tg-file-1",
          kind: "image",
          mime: "image/jpeg",
        },
      }),
    ).rejects.toMatchObject({ nonRetryable: true, type: "ChannelMediaRejected" });
    expect(getFile).not.toHaveBeenCalled();
  });

  it("does not expose the bot token when a download fails", async () => {
    const activities = createTelegramMediaActivities({
      accountId: "primary",
      botToken: "super-secret-token",
      lightspeedEndpoint: "http://lightspeed.test/rpc",
      api: { getFile: async () => ({ file_path: "photo.jpg" }) },
      fetch: vi.fn(async () => {
        throw new Error("request to bot super-secret-token failed");
      }),
    });
    await expect(
      activities.prepareChannelMedia({
        universeId,
        route,
        media: {
          version: 1,
          provider: "telegram",
          fileId: "tg-file-1",
          kind: "image",
          mime: "image/jpeg",
        },
      }),
    ).rejects.not.toThrow(/super-secret-token/);
  });
});
