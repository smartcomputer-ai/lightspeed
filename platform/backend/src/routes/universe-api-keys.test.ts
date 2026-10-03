import { Hono } from "hono";
import { afterEach, expect, it, vi } from "vitest";
import type { ApiVariables, AppContext } from "../context.js";
import { universeRoutes } from "./universes.js";

afterEach(() => vi.unstubAllGlobals());

const universe = { id: "platform-universe", organizationId: "org", lightspeedUniverseId: "33333333-3333-4333-8333-333333333333", gatewayUrl: null };

/// Universe key routes for a caller with `role`, over a core that records
/// each call and a database that answers the universe and membership reads.
function setup(role: string, keys = [{ keyPrefix: "lsk_abcdefgh", assertActor: false, revokedAtMs: null as number | null }]) {
  const calls: { method: string; params: Record<string, unknown> }[] = [];
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
    calls.push({ method: rpc.method, params: rpc.params });
    const apiKey = { keyPrefix: "lsk_abcdefgh", groups: rpc.params.groups };
    return Response.json({ id: rpc.id, result: { result: rpc.method === "deployment/api-keys/list" ? { apiKeys: keys } : { apiKey, secret: "lsk_secret" }, notifications: [] } });
  }));
  const queue: unknown[][] = [[{ universe, slug: "test" }], [{ role }]];
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
  const rotate = (prefix = "lsk_abcdefgh") => app.request(`/platform-universe/api-keys/${prefix}/rotate`, { method: "POST" });
  return { create, rotate, calls, audit };
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

it("refuses a key without groups rather than granting every group", async () => {
  const { create, calls } = setup("admin");
  expect((await create({ displayName: "Agent" })).status).toBe(400);
  expect((await setup("admin").create({ displayName: "Agent", groups: [] })).status).toBe(400);
  expect(calls).toEqual([]);
});

it("keeps universe keys with admins", async () => {
  const { create, calls } = setup("operator");
  expect((await create({ displayName: "Agent", groups: ["session"] })).status).toBe(404);
  expect(calls).toEqual([]);
});

it("rotates only a key belonging to the administered universe", async () => {
  const { rotate, calls, audit } = setup("admin");
  const response = await rotate();
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject({ secret: "lsk_secret" });
  expect(calls).toEqual([
    { method: "deployment/api-keys/list", params: { scope: { kind: "universe", universeId: universe.lightspeedUniverseId } } },
    { method: "deployment/api-keys/rotate", params: { keyPrefix: "lsk_abcdefgh" } },
  ]);
  expect(audit).toHaveBeenCalledWith(expect.objectContaining({ actorId: "caller", action: "key.rotate", targetId: "lsk_abcdefgh", universeId: universe.id }));
});

it.each(["operator", "contributor", "viewer"])("refuses rotation by a %s", async (role) => {
  const { rotate, calls } = setup(role);
  expect((await rotate()).status).toBe(404);
  expect(calls).toEqual([]);
});

it.each([
  ["another universe's key", [], 404],
  ["revoked key", [{ keyPrefix: "lsk_abcdefgh", assertActor: false, revokedAtMs: 10 }], 404],
  ["actor-asserting key", [{ keyPrefix: "lsk_abcdefgh", assertActor: true, revokedAtMs: null }], 403],
] as const)("refuses rotation of %s", async (_name, keys, status) => {
  const { rotate, calls, audit } = setup("admin", [...keys]);
  expect((await rotate()).status).toBe(status);
  expect(calls.map((call) => call.method)).toEqual(["deployment/api-keys/list"]);
  expect(audit).not.toHaveBeenCalled();
});
