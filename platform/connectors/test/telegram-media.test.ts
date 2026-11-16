import { describe, expect, it, vi } from "vitest";
import { CoreClient } from "../src/core/client.js";
import type { TokenSource } from "../src/core/leases.js";
import { createTelegramMediaActivities } from "../src/providers/telegram/media.js";
import { decodeHttpRequest } from "../../test/http-fixture.js";
import { UNIVERSE_A } from "./fixtures.js";

const route = { provider: "telegram" as const, accountId: "primary", chatId: "123" };

function token(value = "secret-token") {
  return { get: async () => value, invalidate: vi.fn<() => void>() } satisfies TokenSource;
}

describe("Telegram media activities", () => {
  it("downloads into Lightspeed CAS under the universe resource URL and returns only a media reference", async () => {
    const bytes = Buffer.from("image bytes");
    const requests: Array<{ url: string; body?: Record<string, unknown>; headers: Headers; universe?: string | undefined }> = [];
    const fetch = vi.fn<typeof globalThis.fetch>(async (input, init) => {
      const url = String(input);
      if (url.startsWith("https://api.telegram.org/")) {
        requests.push({ url, headers: new Headers(init?.headers) });
        return new Response(bytes, { headers: { "content-length": String(bytes.byteLength) } });
      }
      const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
      requests.push({ url, body, headers: new Headers(init?.headers), universe: decodeHttpRequest(input, init).universe });
      return Response.json({
        blobs: [{ blobRef: `sha256:${"a".repeat(64)}`, bytes: bytes.byteLength }],
      });
    });
    const core = new CoreClient({ apiToken: "connector-test-runtime-token-1234567890", endpoint: "http://lightspeed.test/api/v1", fetch });
    const activities = createTelegramMediaActivities({
      universeId: UNIVERSE_A,
      accountId: "primary",
      botToken: token(),
      core: core.forUniverse(UNIVERSE_A),
      api: { getFile: vi.fn(async () => ({ file_path: "photos/photo 1.jpg", file_size: bytes.byteLength })) },
      fetch,
    });

    await expect(
      activities.prepareChannelMedia({
        universeId: UNIVERSE_A,
        route,
        media: { fileId: "tg-file-1", kind: "image", mime: "image/jpeg", name: "photo.jpg", byteSize: bytes.byteLength },
      }),
    ).resolves.toEqual({
      item: { blobRef: `sha256:${"a".repeat(64)}`, kind: "image", mime: "image/jpeg", name: "photo.jpg" },
    });
    expect(requests[0]?.url).toBe("https://api.telegram.org/file/botsecret-token/photos/photo%201.jpg");
    expect(requests[1]?.url).toBe(`http://lightspeed.test/api/v1/universes/${UNIVERSE_A}/blobs/batch`);
    expect(requests[1]?.universe).toBe(UNIVERSE_A);
    expect(requests[1]?.body).toMatchObject({
      blobs: [{ bytesBase64: bytes.toString("base64") }],
    });
  });

  it("rejects the wrong account or universe without contacting Telegram", async () => {
    const getFile = vi.fn(async () => ({ file_path: "photo.jpg" }));
    const activities = createTelegramMediaActivities({
      universeId: UNIVERSE_A,
      accountId: "primary",
      botToken: token(),
      core: new CoreClient({ apiToken: "connector-test-runtime-token-1234567890", endpoint: "http://lightspeed.test/api/v1" }).forUniverse(UNIVERSE_A),
      api: { getFile },
    });
    const media = { fileId: "tg-file-1", kind: "image" as const, mime: "image/jpeg" };
    await expect(
      activities.prepareChannelMedia({ universeId: UNIVERSE_A, route: { ...route, accountId: "other" }, media }),
    ).rejects.toMatchObject({ nonRetryable: true, type: "ChannelMediaRejected" });
    await expect(
      activities.prepareChannelMedia({
        universeId: "00000000-0000-0000-0000-000000000001",
        route,
        media,
      }),
    ).rejects.toMatchObject({ nonRetryable: true, type: "ChannelMediaRejected" });
    expect(getFile).not.toHaveBeenCalled();
  });

  it("does not expose the bot token when a download fails and re-leases on 401", async () => {
    const rejected = token("super-secret-token");
    const activities = createTelegramMediaActivities({
      universeId: UNIVERSE_A,
      accountId: "primary",
      botToken: rejected,
      core: new CoreClient({ apiToken: "connector-test-runtime-token-1234567890", endpoint: "http://lightspeed.test/api/v1" }).forUniverse(UNIVERSE_A),
      api: { getFile: async () => ({ file_path: "photo.jpg" }) },
      fetch: vi.fn(async () => new Response("unauthorized", { status: 401 })),
    });
    const failure = activities.prepareChannelMedia({
      universeId: UNIVERSE_A,
      route,
      media: { fileId: "tg-file-1", kind: "image", mime: "image/jpeg" },
    });
    await expect(failure).rejects.toMatchObject({ type: "ChannelMediaTransferFailed" });
    await expect(failure).rejects.not.toThrow(/super-secret-token/);
    expect(rejected.invalidate).toHaveBeenCalledOnce();
  });
});
