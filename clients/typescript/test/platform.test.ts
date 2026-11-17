import { describe, expect, it, vi } from "vitest";
import { LightspeedClient } from "../src/index.js";

describe("native Platform transport", () => {
  it("keeps deployment prefixes, removes path fields from JSON, and uses the same credential", async () => {
    const fetch = vi.fn<typeof globalThis.fetch>(async (url, init) => {
      expect(String(url)).toBe("https://host/lightspeed/api/v1/platform/organizations/organization%2Fone/universes");
      expect(init?.method).toBe("POST");
      expect(new Headers(init?.headers).get("authorization")).toBe("Bearer personal-key");
      expect(JSON.parse(String(init?.body))).toEqual({ name: "Sandbox", protected: false });
      return Response.json({ universe: { id: "u" } });
    });
    const client = new LightspeedClient({ endpoint: "https://host/lightspeed/api/v1", token: "personal-key", fetch });
    expect(await client.call("platform/universes/create", { organizationId: "organization/one", name: "Sandbox", protected: false })).toEqual({ universe: { id: "u" } });
    expect(fetch).toHaveBeenCalledTimes(1);
  });
  it("rejects path traversal before sending and preserves structured denials", async () => {
    const fetch = vi.fn<typeof globalThis.fetch>(async () => Response.json({ type: "about:blank", title: "Forbidden", status: 403, kind: "forbidden", detail: "Outside key scope" }, { status: 403 }));
    const client = new LightspeedClient({ endpoint: "https://host/api/v1", fetch });
    await expect(client.call("platform/organizations/read", { organizationId: ".." })).rejects.toThrow("Invalid path");
    expect(fetch).not.toHaveBeenCalled();
    await expect(client.call("platform/organizations/read", { organizationId: "organization" })).rejects.toMatchObject({ status: 403, kind: "forbidden", message: "Outside key scope" });
  });
});

it("generated native helpers use the same dynamic call path and object parameters", async () => {
  const fetch = vi.fn<typeof globalThis.fetch>(async () => Response.json({ organizations: [] }));
  const client = new LightspeedClient({ endpoint: "https://host", fetch });
  const call = vi.spyOn(client, "call");
  await client.platform.listOrganizations();
  expect(call).toHaveBeenCalledWith("platform/organizations/list", {}, {});
  await client.platform.readOrganization({ organizationId: "a/b" });
  expect(String(fetch.mock.calls[1]![0])).toBe("https://host/api/v1/platform/organizations/a%2Fb");
});
it("addresses service users and their credentials within an explicit organization", async () => {
  const fetch = vi.fn<typeof globalThis.fetch>(async () => Response.json({ serviceUsers: [] }));
  const client = new LightspeedClient({ endpoint: "https://host", fetch });
  await client.platform.listServiceUsers({ organizationId: "team/a" });
  await client.platform.createServiceUserApiKey({ organizationId: "team/a", serviceUserId: "worker/b", name: "Automation" });
  expect(String(fetch.mock.calls[0]![0])).toBe("https://host/api/v1/platform/organizations/team%2Fa/service-users");
  expect(String(fetch.mock.calls[1]![0])).toBe("https://host/api/v1/platform/organizations/team%2Fa/service-users/worker%2Fb/credentials");
  expect(JSON.parse(String(fetch.mock.calls[1]![1]?.body))).toEqual({ name: "Automation" });
});
it("refuses redirects before rotating credentials", async () => {
  const onSessionToken = vi.fn();
  const client = new LightspeedClient({ endpoint: "https://host", token: "secret", onSessionToken, fetch: async (_url, init) => {
    expect(init?.redirect).toBe("manual"); return new Response(null, { status: 302, headers: { location: "https://other.test", "set-auth-token": "other-secret" } });
  } });
  await expect(client.platform.listOrganizations()).rejects.toThrow("redirects");
  expect(onSessionToken).not.toHaveBeenCalled();
});
it("types Platform discovery without adding identity fields to runtime metadata", async () => {
  const client = new LightspeedClient({ endpoint: "https://host", fetch: async () => Response.json({ runtime: "platform", platform: { principal: { kind: "user", id: "person", name: "Ada", platformAdmin: false }, organizations: [], restrictions: {}, credential: { kind: "personalKey", id: "key" } } }) });
  const connection = await client.initialize();
  if (connection.runtime !== "platform") throw new Error("Expected Platform discovery");
  expect(connection.platform.principal.name).toBe("Ada");
  expect(connection.platform.principal.platformAdmin).toBe(false);
  expect(connection.platform.credential.kind).toBe("personalKey");
});
