import { Hono } from "hono";
import { afterEach, expect, it, vi } from "vitest";
import type { ApiVariables, AppContext } from "../context.js";
import { universeRoutes } from "./universes.js";

afterEach(() => vi.unstubAllGlobals());
function fixture(platformAdmin = true) {
  let slug = "cached";
  const universe = { id: "platform-id", organizationId: "org-id", lightspeedUniverseId: "11111111-1111-4111-8111-111111111111", gatewayUrl: null };
  const reads: unknown[][] = [];
  const calls: { method: string; params: Record<string, unknown> }[] = [];
  const updates: string[] = [];
  const sequence: string[] = [];
  const next = async () => { const value = reads.shift(); if (!value) throw new Error("unexpected database read"); return value; };
  const chain = { from: () => chain, innerJoin: () => chain, leftJoin: () => chain, where: () => chain,
    limit: next, then: (resolve: (value: unknown[]) => unknown) => next().then(resolve) };
  const db = {
    select: () => chain,
    update: () => ({ set: (value: { slug: string }) => ({ where: async () => { slug = value.slug; updates.push(slug); sequence.push("cache"); } }) }),
  };
  const runtime = vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body)); calls.push(rpc); sequence.push("runtime");
    const result = rpc.method === "deployment/universes/list"
      ? { universes: [{ universeId: universe.lightspeedUniverseId, slug: "from-runtime" }] }
      : { universe: { universeId: universe.lightspeedUniverseId, slug: rpc.params.slug } };
    return Response.json({ id: rpc.id, result: { result, notifications: [] } });
  });
  vi.stubGlobal("fetch", runtime);
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => { c.set("session", { user: { id: "user", role: platformAdmin ? "admin" : "user" } } as ApiVariables["session"]); await next(); });
  app.route("/", universeRoutes({ db: { ...db, transaction: async (work: (tx: typeof db) => unknown) => work(db) },
    env: { lightspeedApiUrl: "https://runtime.example/rpc", lightspeedApiKey: "lsk_fixture" },
  } as unknown as AppContext));
  const row = () => ({ universe, slug, role: "admin" });
  const request = (path: string, method = "GET", body?: object) => app.request(path, { method,
    headers: { "content-type": "application/json" }, ...(body ? { body: JSON.stringify(body) } : {}) });
  return { request, row, reads, calls, updates, runtime, sequence };
}

it("serves lists and individual reads from Platform only, even with an unavailable runtime", async () => {
  const f = fixture();
  f.runtime.mockRejectedValue(new TypeError("offline"));
  f.reads.push([f.row()]);
  const listed = await f.request("/");
  expect(listed.status).toBe(200);
  expect(await listed.json()).toMatchObject([{ slug: "cached" }]);
  f.reads.push([f.row()], [{ role: "admin" }]);
  const read = await f.request("/platform-id");
  expect(read.status).toBe(200);
  expect(await read.json()).toMatchObject({ slug: "cached" });
  expect(f.runtime).not.toHaveBeenCalled();
  expect(f.updates).toEqual([]);
});

it("syncs only on an explicit admin action and subsequent reads use the updated cache", async () => {
  const f = fixture(); f.reads.push([f.row()]);
  const synced = await f.request("/sync-slugs", "POST");
  expect(synced.status).toBe(200);
  expect(await synced.json()).toEqual({ updated: 1, skipped: 0 });
  expect(f.calls.map(c => c.method)).toEqual(["deployment/universes/list"]);
  f.reads.push([f.row()]);
  expect(await (await f.request("/")).json()).toMatchObject([{ slug: "from-runtime" }]);
  expect(f.calls).toHaveLength(1);
});

it("rejects non-admin sync and preserves cache when an explicit sync fails", async () => {
  const denied = fixture(false);
  expect((await denied.request("/sync-slugs", "POST")).status).toBe(403);
  expect(denied.runtime).not.toHaveBeenCalled();
  const offline = fixture(); offline.reads.push([offline.row()]);
  offline.runtime.mockRejectedValue(new TypeError("offline"));
  expect((await offline.request("/sync-slugs", "POST")).status).toBe(502);
  expect(offline.updates).toEqual([]);
});

it("renames through runtime first and immediately caches the returned slug", async () => {
  const f = fixture(false);
  f.reads.push([f.row()], [{ role: "admin" }], []);
  const response = await f.request("/platform-id/slug", "PUT", { slug: "renamed" });
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject({ slug: "renamed" });
  expect(f.calls).toMatchObject([{ method: "deployment/universes/slug/put", params: { slug: "renamed", onlyIfUnset: false } }]);
  expect(f.sequence).toEqual(["runtime", "cache"]);
});

it("preserves the cached slug if runtime rejects the rename", async () => {
  const f = fixture(false);
  f.reads.push([f.row()], [{ role: "admin" }], []);
  f.runtime.mockImplementationOnce(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
    return Response.json({ id: rpc.id, error: { code: -32009, message: "slug is already in use", data: { kind: "conflict" } } });
  });
  const response = await f.request("/platform-id/slug", "PUT", { slug: "taken" });
  expect(response.status).toBe(409);
  expect(f.updates).toEqual([]);
});

it("refuses unauthorized renames and known cache collisions before mutating runtime", async () => {
  const denied = fixture(false); denied.reads.push([denied.row()], [{ role: "viewer" }]);
  expect((await denied.request("/platform-id/slug", "PUT", { slug: "other" })).status).toBe(403);
  expect(denied.calls).toEqual([]);
  const collision = fixture(); collision.reads.push([collision.row()], [{ role: "admin" }], [{ id: "other-org" }]);
  expect((await collision.request("/platform-id/slug", "PUT", { slug: "other" })).status).toBe(409);
  expect(collision.calls).toEqual([]);
  expect(collision.updates).toEqual([]);
});
