import { Hono } from "hono";
import { afterEach, expect, it, vi } from "vitest";
import type { ApiVariables, AppContext } from "../context.js";
import { universeRoutes } from "./universes.js";

afterEach(() => vi.unstubAllGlobals());

const universe = { id: "platform-universe", organizationId: "org", lightspeedUniverseId: "33333333-3333-4333-8333-333333333333", gatewayUrl: null };

/// Universe key routes for a caller with `role`, over a core that records
/// each call and a database that answers the universe and membership reads.
function setup(role: string) {
  const calls: { method: string; params: Record<string, unknown> }[] = [];
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
    calls.push({ method: rpc.method, params: rpc.params });
    const apiKey = { keyPrefix: "lsk_abcdefgh", groups: rpc.params.groups };
    return Response.json({ id: rpc.id, result: { result: { apiKey, secret: "lsk_secret" }, notifications: [] } });
  }));
  const queue: unknown[][] = [[{ universe, slug: "test" }], [{ role }]];
  const chain: Record<string, unknown> = {};
  for (const step of ["from", "innerJoin", "where"]) chain[step] = () => chain;
  chain.limit = async () => queue.shift() ?? [];
  const ctx = {
    db: { select: () => chain },
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
  return { create, calls };
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
