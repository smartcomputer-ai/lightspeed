import { Hono } from "hono";
import { afterEach, expect, it, vi } from "vitest";
import type { ApiVariables, AppContext } from "../context.js";
import { universeRoutes } from "./universes.js";

afterEach(() => vi.unstubAllGlobals());

const universe = { id: "platform-universe", organizationId: "org", lightspeedUniverseId: "33333333-3333-4333-8333-333333333333", gatewayUrl: null };

/// Universe key routes for a caller with `role`, over a core that records
/// each call and a database that answers the universe and membership reads.
/// The core holds `keys` and refuses a create that chooses one of their ids.
function setup(role: string, keys = [{ keyId: "agent", keyPrefix: "lsk_abcdefgh", assertActor: false, revokedAtMs: null as number | null }]) {
  const calls: { method: string; params: Record<string, unknown> }[] = [];
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
    calls.push({ method: rpc.method, params: rpc.params });
    if (rpc.method === "deployment/api-keys/create" && keys.some((key) => key.keyId === rpc.params.keyId)) {
      return Response.json({ id: rpc.id, error: { code: -32603, message: `api key id is already in use: ${rpc.params.keyId}`, data: { kind: "conflict" } } });
    }
    const apiKey = { keyId: rpc.params.keyId ?? "agent", keyPrefix: "lsk_abcdefgh", groups: rpc.params.groups };
    return Response.json({ id: rpc.id, result: { result: rpc.method === "deployment/api-keys/list" ? { apiKeys: keys } : { apiKey, secret: "lsk_secret" }, notifications: [] } });
  }));
  const queue: unknown[][] = [[{ universe, handle: "test" }], [{ role }]];
  const chain: Record<string, unknown> = {};
  for (const step of ["from", "innerJoin", "where"]) chain[step] = () => chain;
  chain.limit = async () => queue.shift() ?? [];
  const audit = vi.fn(async () => undefined);
  const ctx = {
    db: { insert: () => ({ values: audit }), select: () => chain },
    env: { lightspeedApiUrl: "https://core.example/rpc", lightspeedApiKey: "lsk_platform" },
  } as unknown as AppContext;
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    c.set("session", { user: { id: "caller" } } as ApiVariables["session"]);
    await next();
  });
  app.route("/", universeRoutes(ctx));
  const create = (body: unknown) => app.request("/platform-universe/api-keys", {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body),
  });
  const rotate = (keyId = "agent") => app.request(`/platform-universe/api-keys/${keyId}/rotate`, { method: "POST" });
  const revoke = (keyId = "agent") => app.request(`/platform-universe/api-keys/${keyId}`, { method: "DELETE" });
  return { create, rotate, revoke, calls, audit };
}

it("mints a universe key with only the groups the admin chose", async () => {
  const { create, calls } = setup("admin");
  const response = await create({ displayName: "Agent", groups: ["session", "blobs/put"] });
  expect(response.status).toBe(201);
  expect(calls).toEqual([{
    method: "deployment/api-keys/create",
    params: {
      scope: { kind: "universe", universeId: universe.lightspeedUniverseId },
      displayName: "Agent",
      groups: ["session", "blobs/put"],
      assertActor: false,
    },
  }]);
});

it("mints a key under the id its creator chose", async () => {
  const { create, calls, audit } = setup("admin");
  const response = await create({ displayName: "CI", groups: ["session"], keyId: " ci " });
  expect(response.status).toBe(201);
  expect(calls.map((call) => call.method)).toEqual(["deployment/api-keys/create"]);
  expect(calls[0]!.params).toMatchObject({ keyId: "ci", displayName: "CI" });
  expect(audit).toHaveBeenCalledWith(expect.objectContaining({ action: "key.create", targetId: "ci", universeId: universe.id }));
});

it.each([
  ["a live key", null, 'key ID "agent" is already in use; leave it blank to generate a new ID or choose another'],
  ["a revoked key", 10, 'key ID "agent" belongs to a revoked key and cannot be reused; leave it blank to generate a new ID or choose another'],
] as const)("refuses a chosen id held by %s", async (_name, revokedAtMs, message) => {
  const { create, calls, audit } = setup("admin", [{ keyId: "agent", keyPrefix: "lsk_abcdefgh", assertActor: false, revokedAtMs }]);
  const response = await create({ displayName: "Agent", groups: ["session"], keyId: "agent" });
  expect(response.status).toBe(409);
  expect(await response.json()).toEqual({ error: message });
  const scope = { kind: "universe", universeId: universe.lightspeedUniverseId };
  expect(calls.map((call) => call.method)).toEqual(["deployment/api-keys/create", "deployment/api-keys/list"]);
  expect(calls[1]!.params).toEqual({ scope });
  expect(audit).not.toHaveBeenCalled();
});

it.each([" ", "k".repeat(129)])("refuses the malformed key id %j before asking the runtime", async (keyId) => {
  const { create, calls } = setup("admin");
  expect((await create({ displayName: "Agent", groups: ["session"], keyId })).status).toBe(400);
  expect(calls).toEqual([]);
});

it("refuses a key without groups rather than granting every group", async () => {
  const { create, calls } = setup("admin");
  expect((await create({ displayName: "Agent" })).status).toBe(400);
  expect((await setup("admin").create({ displayName: "Agent", groups: [] })).status).toBe(400);
  expect(calls).toEqual([]);
});

it("keeps universe keys with admins", async () => {
  const { create, calls } = setup("operator");
  expect((await create({ displayName: "Agent", groups: ["session"] })).status).toBe(404);
  const chosen = setup("operator");
  expect((await chosen.create({ displayName: "Agent", groups: ["session"], keyId: "ci" })).status).toBe(404);
  expect(chosen.calls).toEqual([]);
  expect(calls).toEqual([]);
});

it("rotates only a key belonging to the administered universe", async () => {
  const { rotate, calls, audit } = setup("admin");
  const response = await rotate();
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject({ secret: "lsk_secret" });
  expect(calls).toEqual([
    { method: "deployment/api-keys/list", params: { scope: { kind: "universe", universeId: universe.lightspeedUniverseId } } },
    { method: "deployment/api-keys/rotate", params: { scope: { kind: "universe", universeId: universe.lightspeedUniverseId }, keyId: "agent" } },
  ]);
  expect(audit).toHaveBeenCalledWith(expect.objectContaining({ actorId: "caller", action: "key.rotate", targetId: "agent", universeId: universe.id }));
});

it("revokes a key by its id within the administered universe only", async () => {
  const { revoke, calls, audit } = setup("admin");
  expect((await revoke()).status).toBe(200);
  expect(calls).toEqual([
    { method: "deployment/api-keys/revoke", params: { scope: { kind: "universe", universeId: universe.lightspeedUniverseId }, keyId: "agent" } },
  ]);
  expect(audit).toHaveBeenCalledWith(expect.objectContaining({ action: "key.revoke", targetId: "agent", universeId: universe.id }));
  expect((await setup("operator").revoke()).status).toBe(404);
});

it.each(["operator", "contributor", "viewer"])("refuses rotation by a %s", async (role) => {
  const { rotate, calls } = setup(role);
  expect((await rotate()).status).toBe(404);
  expect(calls).toEqual([]);
});

it.each([
  ["another universe's key", [], 404],
  ["revoked key", [{ keyId: "agent", keyPrefix: "lsk_abcdefgh", assertActor: false, revokedAtMs: 10 }], 404],
  ["actor-asserting key", [{ keyId: "agent", keyPrefix: "lsk_abcdefgh", assertActor: true, revokedAtMs: null }], 403],
] as const)("refuses rotation of %s", async (_name, keys, status) => {
  const { rotate, calls, audit } = setup("admin", [...keys]);
  expect((await rotate()).status).toBe(status);
  expect(calls.map((call) => call.method)).toEqual(["deployment/api-keys/list"]);
  expect(audit).not.toHaveBeenCalled();
});
