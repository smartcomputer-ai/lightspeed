import { Hono } from "hono";
import { expect, it } from "vitest";
import { effectiveFeatures, mergeFeatureOverrides } from "@lightspeed/platform-shared";
import type { ApiVariables, AppContext } from "../context.js";
import { universeRoutes } from "./universes.js";

/// Universe routes over a database that answers each read, in order: the
/// universe (with what it stored), then the caller's membership, then
/// whatever an update returns. Every `set` payload is recorded.
function setup(role: string, stored: Record<string, boolean>) {
  const universe = {
    id: "platform-universe",
    organizationId: "org",
    lightspeedUniverseId: "33333333-3333-4333-8333-333333333333",
    gatewayUrl: null,
    features: stored,
  };
  const sets: Record<string, unknown>[] = [];
  const queue: unknown[][] = [[{ universe, slug: "test" }], [{ role }]];
  const next = async () => {
    const rows = queue.shift();
    if (!rows) throw new Error("unexpected database read");
    return rows;
  };
  const chain: Record<string, unknown> = {};
  for (const step of ["from", "innerJoin", "leftJoin", "where"]) chain[step] = () => chain;
  chain.set = (payload: Record<string, unknown>) => {
    sets.push(payload);
    queue.push([{ ...universe, ...payload }]);
    return chain;
  };
  chain.limit = next;
  chain.returning = next;
  chain.then = (resolve: (rows: unknown[]) => unknown, reject: (error: unknown) => unknown) => next().then(resolve, reject);
  const db = {
    select: () => chain,
    update: () => chain,
    transaction: async (work: (tx: unknown) => unknown) => work(db),
  };
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    c.set("session", { user: { id: "caller" } } as ApiVariables["session"]);
    await next();
  });
  app.route("/", universeRoutes({ db } as unknown as AppContext));
  const request = (method = "GET", body?: unknown) => app.request("/platform-universe", {
    method, headers: { "content-type": "application/json" }, ...(body ? { body: JSON.stringify(body) } : {}),
  });
  return { request, sets };
}

it("works out what is on from defaults, switches and requirements", () => {
  expect(effectiveFeatures({})).toEqual({ bots: true, channels: true });
  expect(effectiveFeatures(null)).toEqual({ bots: true, channels: true });
  expect(effectiveFeatures({ channels: false })).toEqual({ bots: true, channels: false });
  // Channels need bots; their own switch comes back with bots.
  expect(effectiveFeatures({ bots: false })).toEqual({ bots: false, channels: false });
  expect(effectiveFeatures({ nonsense: true })).toEqual({ bots: true, channels: true });
});

it("stores only switches away from their default", () => {
  expect(mergeFeatureOverrides({}, { bots: false })).toEqual({ bots: false });
  expect(mergeFeatureOverrides({ bots: false }, { bots: true })).toEqual({});
  expect(mergeFeatureOverrides({ bots: false }, { channels: false })).toEqual({ bots: false, channels: false });
});

it("shows every member what is on, not what is stored", async () => {
  const response = await setup("viewer", { bots: false }).request();
  expect(await response.json()).toMatchObject({ features: { bots: false, channels: false } });
});

it.each(["viewer", "contributor", "operator"])("keeps switching features with admins, refusing a %s", async (role) => {
  const { request, sets } = setup(role, {});
  expect((await request("PATCH", { features: { bots: false } })).status).toBe(403);
  expect(sets).toEqual([]);
});

it("lets an admin switch a feature, merged into what was stored", async () => {
  const { request, sets } = setup("admin", { channels: false });
  const response = await request("PATCH", { features: { bots: false } });
  expect(response.status).toBe(200);
  expect(sets).toEqual([{ features: { channels: false, bots: false } }]);
  expect(await response.json()).toMatchObject({ features: { bots: false, channels: false } });
});

it("refuses a feature it does not know", async () => {
  const { request, sets } = setup("admin", {});
  expect((await request("PATCH", { features: { telepathy: true } })).status).toBe(400);
  expect(sets).toEqual([]);
});
