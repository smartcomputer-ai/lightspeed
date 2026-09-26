import { Hono } from "hono";
import { afterEach, expect, it, vi } from "vitest";
import { schema } from "@lightspeed/platform-db";
import type { ApiVariables, AppContext } from "../context.js";
import { apiKeyAdminRoutes } from "./api-keys-admin.js";

afterEach(() => vi.unstubAllGlobals());

const universe = { id: "11111111-1111-4111-8111-111111111111", lightspeedUniverseId: "22222222-2222-4222-8222-222222222222" };

/// Admin key routes over a core that records each call and a database that
/// knows one universe.
function setup(platformRole: string | undefined) {
  const calls: { method: string; params: Record<string, unknown>; headers: Headers }[] = [];
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
    calls.push({ method: rpc.method, params: rpc.params, headers: new Headers(init.headers) });
    const apiKey = { keyPrefix: "lsk_abcdefgh" };
    const result = rpc.method === "deployment/api-keys/create" ? { apiKey, secret: "lsk_secret" }
      : rpc.method === "deployment/api-keys/list" ? { apiKeys: [apiKey] } : { apiKey };
    return Response.json({ id: rpc.id, result: { result, notifications: [] } });
  }));
  const query = { from: () => query, where: () => query, limit: async () => [universe] };
  const ctx = {
    db: { select: (fields: unknown) => { expect(fields).toHaveProperty("lightspeedUniverseId", schema.universes.lightspeedUniverseId); return query; } },
    env: { lightspeedApiUrl: "https://core.example/rpc", lightspeedApiKey: "lsk_platform" },
  } as unknown as AppContext;
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    c.set("session", { user: { id: "admin", role: platformRole } } as ApiVariables["session"]);
    await next();
  });
  app.route("/", apiKeyAdminRoutes(ctx));
  const request = (method: string, path: string, body?: unknown) => app.request(path, {
    method, headers: { "content-type": "application/json" }, ...(body ? { body: JSON.stringify(body) } : {}),
  });
  return { request, calls };
}

it("is for platform admins only", async () => {
  const { request, calls } = setup(undefined);
  expect((await request("GET", "/api-keys")).status).toBe(403);
  expect((await request("POST", "/api-keys", { displayName: "x", scope: { kind: "deployment" } })).status).toBe(403);
  expect((await request("DELETE", "/api-keys/lsk_abcdefgh")).status).toBe(403);
  expect(calls).toEqual([]);
});

it("mints a key for a Platform universe with chosen groups, through the deployment key alone", async () => {
  const { request, calls } = setup("admin");
  const response = await request("POST", "/api-keys", {
    displayName: "Connector", scope: { kind: "universe", universeId: universe.id }, groups: ["channels/inbound", "blobs/put"],
  });
  expect(response.status).toBe(201);
  expect(await response.json()).toMatchObject({ secret: "lsk_secret" });
  expect(calls[0]).toMatchObject({
    method: "deployment/api-keys/create",
    params: {
      scope: { kind: "universe", universeId: universe.lightspeedUniverseId },
      displayName: "Connector", groups: ["channels/inbound", "blobs/put"], assertActor: false,
    },
  });
  expect(calls[0]!.headers.get("x-lightspeed-universe")).toBeNull();
  expect(calls[0]!.headers.get("x-lightspeed-actor")).toBeNull();
});

it("mints a deployment key that may assert actors, and lists and revokes keys", async () => {
  const { request, calls } = setup("admin");
  expect((await request("POST", "/api-keys", { displayName: "Gate", scope: { kind: "deployment" }, assertActor: true })).status).toBe(201);
  expect(calls[0]!.params).toEqual({ scope: { kind: "deployment" }, displayName: "Gate", assertActor: true });
  expect(await (await request("GET", "/api-keys")).json()).toEqual([{ keyPrefix: "lsk_abcdefgh" }]);
  expect((await request("DELETE", "/api-keys/lsk_abcdefgh")).status).toBe(200);
  expect(calls.map((call) => call.method)).toEqual(["deployment/api-keys/create", "deployment/api-keys/list", "deployment/api-keys/revoke"]);
});
