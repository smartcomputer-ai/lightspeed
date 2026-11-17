import { userSubject } from "../../test/subject-fixture.js";
import { Hono } from "hono";
import { expect, it } from "vitest";
import { effectiveFeatures, featureOfMethod, mergeFeatureOverrides } from "@lightspeed-ai/platform-shared";
import type { ApiVariables, AppContext } from "../context.js";
import { universeRoutes } from "./universes.js";

/// Universe routes over a database that answers each read, in order: the
/// universe (with what it stored), then the caller's membership, then
/// whatever an update returns. Every `set` payload is recorded.
function setup(role: string, stored: Record<string, boolean>) {
  const universe = { handle: "test", status: "active", runtimeConfiguration: {}, accessPolicy: {},
    id: "platform-universe",
    organizationId: "org",
    lightspeedUniverseId: "33333333-3333-4333-8333-333333333333",

    features: stored,
  };
  const sets: Record<string, unknown>[] = [];
  const queue: unknown[][] = [[universe], [{ id: "org", enabled: true, membershipPolicy: {} }], [{ role: "member" }], [{ role }]];
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
    c.set("auth", userSubject(c.get("session").user.id, c.get("session").user.role === "owner"));
    await next();
  });
  app.route("/", universeRoutes({ db, env: {} } as unknown as AppContext));
  const request = (method = "GET", body?: unknown) => app.request("/platform-universe", {
    method, headers: { "content-type": "application/json" }, ...(body ? { body: JSON.stringify(body) } : {}),
  });
  return { request, sets };
}

it("works out what is on from defaults and switches", () => {
  // Nothing stored: every feature is off.
  expect(effectiveFeatures({})).toEqual({ bots: false });
  expect(effectiveFeatures(null)).toEqual({ bots: false });
  expect(effectiveFeatures({ bots: true })).toEqual({ bots: true });
});

it("skips a stored entry it cannot read without touching the others", () => {
  expect(effectiveFeatures({ nonsense: true })).toEqual({ bots: false });
  expect(effectiveFeatures({ bots: true, nonsense: true })).toEqual({ bots: true });
  expect(effectiveFeatures("on")).toEqual({ bots: false });
  expect(mergeFeatureOverrides({ nonsense: true }, { bots: true })).toEqual({ bots: true });
});

it("stores only switches away from their default", () => {
  expect(mergeFeatureOverrides({}, { bots: true })).toEqual({ bots: true });
  expect(mergeFeatureOverrides({ bots: true }, { bots: false })).toEqual({});
});

it("names the feature a runtime method belongs to", () => {
  expect(featureOfMethod("bots/list")).toBe("bots");
  expect(featureOfMethod("bots/triggers/put")).toBe("bots");
  for (const method of ["session/start", "profiles/list", "bots", "robots/list"]) {
    expect(featureOfMethod(method), method).toBeNull();
  }
});

it("shows a universe that stored nothing with every feature off", async () => {
  const response = await setup("viewer", {}).request();
  expect(await response.json()).toMatchObject({ features: { bots: false } });
});

it("shows every member what is on, not what is stored", async () => {
  const response = await setup("viewer", { bots: true }).request();
  expect(await response.json()).toMatchObject({ features: { bots: true } });
});

it.each(["viewer", "contributor", "operator"])("keeps switching features with admins, refusing a %s", async (role) => {
  const { request, sets } = setup(role, {});
  expect((await request("PATCH", { features: { bots: true } })).status).toBe(403);
  expect(sets).toEqual([]);
});

it("lets an admin switch a feature on, merged into what was stored", async () => {
  const { request, sets } = setup("owner", {});
  const response = await request("PATCH", { features: { bots: true } });
  expect(response.status).toBe(200);
  expect(sets).toEqual([{ features: { bots: true } }]);
  expect(await response.json()).toMatchObject({ features: { bots: true } });
});

it("stores nothing for a switch an admin turns back off", async () => {
  const { request, sets } = setup("owner", { bots: true });
  const response = await request("PATCH", { features: { bots: false } });
  expect(response.status).toBe(200);
  expect(sets).toEqual([{ features: {} }]);
  expect(await response.json()).toMatchObject({ features: { bots: false } });
});

it("refuses a feature it does not know", async () => {
  const { request, sets } = setup("owner", {});
  expect((await request("PATCH", { features: { telepathy: true } })).status).toBe(400);
  expect(sets).toEqual([]);
});
