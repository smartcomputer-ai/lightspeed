import { Hono } from "hono";
import { afterEach, expect, it, vi } from "vitest";
import { schema } from "@lightspeed-ai/platform-db";
import type { ApiVariables, AppContext } from "../context.js";
import { apiKeyAdminRoutes } from "./api-keys-admin.js";

afterEach(() => vi.unstubAllGlobals());

const universe = { id: "11111111-1111-4111-8111-111111111111", lightspeedUniverseId: "22222222-2222-4222-8222-222222222222" };

/// Admin key routes over a core that records each call and a database that
/// knows one universe. The core holds the key `connector` in every scope and
/// refuses a create that chooses its id.
function setup(platformRole: string | undefined) {
  const calls: { method: string; params: Record<string, unknown>; headers: Headers }[] = [];
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
    calls.push({ method: rpc.method, params: rpc.params, headers: new Headers(init.headers) });
    const apiKey = { keyId: "connector", keyPrefix: "lsk_abcdefgh" };
    if (rpc.method === "deployment/api-keys/create" && rpc.params.keyId === apiKey.keyId) {
      return Response.json({ id: rpc.id, error: { code: -32603, message: "api key id is already in use: connector", data: { kind: "conflict" } } });
    }
    const result = rpc.method === "deployment/api-keys/create" ? { apiKey: { ...apiKey, keyId: rpc.params.keyId ?? apiKey.keyId }, secret: "lsk_secret" }
      : rpc.method === "deployment/api-keys/rotate" ? { apiKey, secret: "lsk_secret" }
      : rpc.method === "deployment/api-keys/list" ? { apiKeys: [apiKey] } : { apiKey };
    return Response.json({ id: rpc.id, result: { result, notifications: [] } });
  }));
  const query = { from: () => query, where: () => query, limit: async () => [universe] };
  const audit = vi.fn(async () => undefined);
  const ctx = {
    db: { insert: () => ({ values: audit }), select: (fields: unknown) => { expect(fields).toHaveProperty("lightspeedUniverseId", schema.universes.lightspeedUniverseId); return query; } },
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
  return { request, calls, audit };
}

it("is for platform admins only", async () => {
  const { request, calls } = setup(undefined);
  expect((await request("GET", "/api-keys")).status).toBe(403);
  expect((await request("POST", "/api-keys", { displayName: "x", scope: { kind: "deployment" }, groups: "all" })).status).toBe(403);
  expect((await request("POST", "/api-keys", { displayName: "x", scope: { kind: "deployment" }, groups: "all", keyId: "gate" })).status).toBe(403);
  expect((await request("DELETE", "/api-keys/connector")).status).toBe(403);
  expect((await request("POST", "/api-keys/connector/rotate")).status).toBe(403);
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
  expect((await request("POST", "/api-keys", { displayName: "Gate", scope: { kind: "deployment" }, groups: "all", assertActor: true })).status).toBe(201);
  expect(calls[0]!.params).toEqual({ scope: { kind: "deployment" }, displayName: "Gate", groups: "all", assertActor: true });
  expect(await (await request("GET", "/api-keys")).json()).toEqual([{ keyId: "connector", keyPrefix: "lsk_abcdefgh" }]);
  expect((await request("DELETE", "/api-keys/connector")).status).toBe(200);
  expect(calls.map((call) => call.method)).toEqual(["deployment/api-keys/create", "deployment/api-keys/list", "deployment/api-keys/revoke"]);
  expect(calls[2]!.params).toEqual({ scope: { kind: "deployment" }, keyId: "connector" });
});

it("refuses a key whose groups are not stated, before the runtime is asked anything", async () => {
  const { request, calls, audit } = setup("admin");
  const key = { displayName: "Gate", scope: { kind: "deployment" } };
  // Leaving the groups out never means every group.
  for (const groups of [undefined, null, [], "everything", "ALL", [""]]) {
    expect((await request("POST", "/api-keys", { ...key, groups })).status, JSON.stringify(groups)).toBe(400);
  }
  expect(calls).toEqual([]);
  expect(audit).not.toHaveBeenCalled();
});

it("mints a key under the id its creator chose, in the chosen scope", async () => {
  const { request, calls, audit } = setup("admin");
  const response = await request("POST", "/api-keys", { displayName: "Gate", scope: { kind: "deployment" }, groups: ["session"], keyId: " gate " });
  expect(response.status).toBe(201);
  expect(calls).toHaveLength(1);
  expect(calls[0]!.params).toEqual({ scope: { kind: "deployment" }, displayName: "Gate", groups: ["session"], assertActor: false, keyId: "gate" });
  expect(audit).toHaveBeenCalledWith(expect.objectContaining({ action: "key.create", targetId: "gate" }));
});

it("refuses a chosen id that a key of the scope already holds", async () => {
  const { request, calls, audit } = setup("admin");
  const response = await request("POST", "/api-keys", {
    displayName: "Connector", scope: { kind: "universe", universeId: universe.id }, groups: ["session"], keyId: "connector",
  });
  expect(response.status).toBe(409);
  expect(await response.json()).toEqual({
    error: 'key ID "connector" is already in use; leave it blank to generate a new ID or choose another',
  });
  // The holder is looked up among the keys of the same scope only.
  expect(calls.map((call) => call.method)).toEqual(["deployment/api-keys/create", "deployment/api-keys/list"]);
  expect(calls[1]!.params).toEqual({ scope: { kind: "universe", universeId: universe.lightspeedUniverseId } });
  expect(audit).not.toHaveBeenCalled();
  expect((await request("POST", "/api-keys", { displayName: "x", scope: { kind: "deployment" }, groups: ["session"], keyId: "k".repeat(129) })).status).toBe(400);
  expect(calls).toHaveLength(2);
});

it("rotates an admin key and audits only its identifiers", async () => {
  const { request, calls, audit } = setup("admin");
  const response = await request("POST", "/api-keys/connector/rotate");
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject({ apiKey: { keyPrefix: "lsk_abcdefgh" }, secret: "lsk_secret" });
  expect(calls).toEqual([expect.objectContaining({ method: "deployment/api-keys/rotate", params: { scope: { kind: "deployment" }, keyId: "connector" } })]);
  expect(audit).toHaveBeenCalledWith({ actorId: "admin", action: "key.rotate", targetId: "connector", details: { newKeyPrefix: "lsk_abcdefgh" }, outcome: "success" });
});

it("addresses a universe's key by its id within that universe", async () => {
  const { request, calls } = setup("admin");
  const scoped = `?universeId=${universe.lightspeedUniverseId}`;
  expect((await request("POST", `/api-keys/ci/rotate${scoped}`)).status).toBe(200);
  expect((await request("DELETE", `/api-keys/ci${scoped}`)).status).toBe(200);
  const scope = { kind: "universe", universeId: universe.lightspeedUniverseId };
  expect(calls.map((call) => call.params)).toEqual([{ scope, keyId: "ci" }, { scope, keyId: "ci" }]);
  // A malformed scope is refused before the runtime is asked anything.
  expect((await request("DELETE", "/api-keys/ci?universeId=nope")).status).toBe(400);
  expect(calls).toHaveLength(2);
});
