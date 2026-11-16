import { describe, expect, it, vi } from "vitest";
import { LightspeedClient } from "../src/index.js";

describe("native Platform transport", () => {
  it("keeps deployment prefixes, removes path fields from JSON, and uses the same credential", async () => {
    const fetch = vi.fn<typeof globalThis.fetch>(async (url, init) => {
      expect(String(url)).toBe("https://host/lightspeed/api/v1/platform/accounts/account%2Fone/universes");
      expect(init?.method).toBe("POST");
      expect(new Headers(init?.headers).get("authorization")).toBe("Bearer personal-key");
      expect(JSON.parse(String(init?.body))).toEqual({ name: "Sandbox", protected: false });
      return Response.json({ universe: { id: "u" } });
    });
    const client = new LightspeedClient({ endpoint: "https://host/lightspeed/api/v1", token: "personal-key", fetch });
    expect(await client.callPlatform("platform/universes/create", { accountId: "account/one", name: "Sandbox", protected: false })).toEqual({ universe: { id: "u" } });
    expect(fetch).toHaveBeenCalledTimes(1);
  });
  it("rejects path traversal before sending and preserves structured denials", async () => {
    const fetch = vi.fn<typeof globalThis.fetch>(async () => Response.json({ type: "about:blank", title: "Forbidden", status: 403, kind: "forbidden", detail: "Outside key scope" }, { status: 403 }));
    const client = new LightspeedClient({ endpoint: "https://host/api/v1", fetch });
    await expect(client.callPlatform("platform/accounts/read", { accountId: ".." })).rejects.toThrow("Invalid path");
    expect(fetch).not.toHaveBeenCalled();
    await expect(client.callPlatform("platform/accounts/read", { accountId: "account" })).rejects.toMatchObject({ status: 403, kind: "forbidden", message: "Outside key scope" });
  });
});
