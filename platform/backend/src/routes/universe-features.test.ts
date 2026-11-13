import { Hono } from "hono";
import { expect, it } from "vitest";
import { effectiveFeatures, featureOfMethod, mergeFeatureOverrides } from "@lightspeed-ai/platform-shared";
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
  const queue: unknown[][] = [[{ universe, handle: "test" }], [{ role }]];
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
  app.route("/", universeRoutes({ db, env: {} } as unknown as AppContext));
  const request = (method = "GET", body?: unknown) => app.request("/platform-universe", {
    method, headers: { "content-type": "application/json" }, ...(body ? { body: JSON.stringify(body) } : {}),
  });
  return { request, sets };
}

it("works out what is on from defaults, switches and requirements", () => {
  // Nothing stored: every feature is off.
  expect(effectiveFeatures({})).toEqual({ bots: false, channels: false });
  expect(effectiveFeatures(null)).toEqual({ bots: false, channels: false });
  expect(effectiveFeatures({ bots: true })).toEqual({ bots: true, channels: false });
  expect(effectiveFeatures({ bots: true, channels: true })).toEqual({ bots: true, channels: true });
  // Channels need bots; their own switch comes back with bots.
  expect(effectiveFeatures({ channels: true })).toEqual({ bots: false, channels: false });
  expect(effectiveFeatures({ bots: false, channels: true })).toEqual({ bots: false, channels: false });
});

it("skips a stored entry it cannot read without touching the others", () => {
  expect(effectiveFeatures({ nonsense: true })).toEqual({ bots: false, channels: false });
  expect(effectiveFeatures({ bots: true, nonsense: true })).toEqual({ bots: true, channels: false });
  expect(effectiveFeatures({ bots: true, channels: "yes" })).toEqual({ bots: true, channels: false });
  expect(effectiveFeatures("on")).toEqual({ bots: false, channels: false });
  expect(mergeFeatureOverrides({ bots: true, nonsense: true }, { channels: true })).toEqual({ bots: true, channels: true });
});

it("stores only switches away from their default", () => {
  expect(mergeFeatureOverrides({}, { bots: true })).toEqual({ bots: true });
  expect(mergeFeatureOverrides({ bots: true }, { bots: false })).toEqual({});
  expect(mergeFeatureOverrides({ bots: true }, { channels: true })).toEqual({ bots: true, channels: true });
  // Switching bots off leaves the channels switch stored, for when bots return.
  expect(mergeFeatureOverrides({ bots: true, channels: true }, { bots: false })).toEqual({ channels: true });
});

it("names the feature a runtime method belongs to", () => {
  expect(featureOfMethod("bots/list")).toBe("bots");
  expect(featureOfMethod("bots/triggers/put")).toBe("bots");
  expect(featureOfMethod("channels/accounts/list")).toBe("channels");
  for (const method of ["session/start", "profiles/list", "deployment/channels/accounts/list", "bots", "robots/list"]) {
    expect(featureOfMethod(method), method).toBeNull();
  }
});

it("shows a universe that stored nothing with every feature off", async () => {
  const response = await setup("viewer", {}).request();
  expect(await response.json()).toMatchObject({ features: { bots: false, channels: false } });
});

it("shows every member what is on, not what is stored", async () => {
  const response = await setup("viewer", { channels: true }).request();
  expect(await response.json()).toMatchObject({ features: { bots: false, channels: false } });
});

it.each(["viewer", "contributor", "operator"])("keeps switching features with admins, refusing a %s", async (role) => {
  const { request, sets } = setup(role, {});
  expect((await request("PATCH", { features: { bots: true } })).status).toBe(403);
  expect(sets).toEqual([]);
});

it("lets an admin switch a feature on, merged into what was stored", async () => {
  const { request, sets } = setup("admin", { bots: true });
  const response = await request("PATCH", { features: { channels: true } });
  expect(response.status).toBe(200);
  expect(sets).toEqual([{ features: { bots: true, channels: true } }]);
  expect(await response.json()).toMatchObject({ features: { bots: true, channels: true } });
});

it("stores nothing for a switch an admin turns back off", async () => {
  const { request, sets } = setup("admin", { bots: true, channels: true });
  const response = await request("PATCH", { features: { bots: false } });
  expect(response.status).toBe(200);
  expect(sets).toEqual([{ features: { channels: true } }]);
  expect(await response.json()).toMatchObject({ features: { bots: false, channels: false } });
});

it("refuses a feature it does not know", async () => {
  const { request, sets } = setup("admin", {});
  expect((await request("PATCH", { features: { telepathy: true } })).status).toBe(400);
  expect(sets).toEqual([]);
});
