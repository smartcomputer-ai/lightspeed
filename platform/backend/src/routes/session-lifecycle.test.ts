import { Hono } from "hono";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ApiVariables, AppContext } from "../context.js";
import { gatewayRoutes } from "./gateway.js";
import { sessionAdminRoutes } from "./session-admin.js";

const auth = vi.hoisted(() => ({ role: "admin", platformAdmin: false }));
vi.mock("./universes.js", () => ({ universeForSession: vi.fn(async () => ({
  universe: { id: "universe", lightspeedUniverseId: "runtime-universe", gatewayUrl: null },
  slug: "test", role: auth.role, member: { userId: "member", role: auth.role },
})) }));
beforeEach(() => { auth.role = "admin"; auth.platformAdmin = false; });
afterEach(() => vi.unstubAllGlobals());

function fixture({ failMutation = false, purgedIds = ["s1", "child"] }: { failMutation?: boolean; purgedIds?: string[] } = {}) {
  const audits = vi.fn(async () => undefined);
  const rpcCalls: string[] = [];
  const rpcParams: Record<string, unknown>[] = [];
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
    rpcCalls.push(rpc.method);
    rpcParams.push(rpc.params);
    if (failMutation && rpc.method !== "session/read") {
      return Response.json({ id: rpc.id, error: { code: -32009, message: "failed", data: { kind: "conflict" } } });
    }
    const result = rpc.method === "deployment/sessions/purge" ? { deletedSessionIds: purgedIds }
      : rpc.method === "deployment/sessions/deleted/list" ? { sessions: [], nextAfter: null }
      : { session: { id: "s1", access: { visibility: "universe" } }, deletedSessionCount: 2 };
    return Response.json({ id: rpc.id, result: { result, notifications: [] } });
  }));
  const db = { insert: () => ({ values: audits }), select: () => ({ from: () => ({ where: () => ({ limit: async () => [{ id: "universe", lightspeedUniverseId: "runtime-universe" }] }) }) }) };
  const ctx = { db, env: { lightspeedApiUrl: "https://engine.example/rpc", lightspeedApiKey: "lsk_fixture" } } as unknown as AppContext;
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    c.set("session", { user: { id: "member", role: auth.platformAdmin ? "admin" : "user" } } as ApiVariables["session"]);
    await next();
  });
  app.route("/universes", gatewayRoutes(ctx));
  app.route("/admin", sessionAdminRoutes(ctx));
  const call = (path: string, method = "POST", body: unknown = {}) => app.request(path, { method,
    headers: { "content-type": "application/json" }, ...(method === "POST" ? { body: JSON.stringify(body) } : {}),
  });
  return { call, audits, rpcCalls, rpcParams };
}

it.each([false, true])("closes with force=%s without writing an audit record", async (force) => {
  const f = fixture();
  expect((await f.call("/universes/universe/sessions/s1/close", "POST", { force })).status).toBe(200);
  expect(f.rpcCalls).toContain("session/close");
  expect(f.audits).not.toHaveBeenCalled();
});
it.each(["operator", "admin"])("%s always soft deletes even if the URL requests permanent deletion", async (role) => {
  auth.role = role;
  const f = fixture();
  expect((await f.call("/universes/universe/sessions/s1?cascade=true&softDelete=false", "DELETE")).status).toBe(200);
  expect(f.rpcCalls).toContain("session/delete");
  expect(f.rpcParams[f.rpcCalls.indexOf("session/delete")]).toMatchObject({ sessionId: "s1", cascade: true, softDelete: true });
  expect(f.audits).not.toHaveBeenCalled();
});
it("does not record successful purge when the runtime rejects it", async () => {
  auth.platformAdmin = true;
  const f = fixture({ failMutation: true });
  expect((await f.call("/admin/universes/universe/sessions/s1/purge")).status).not.toBe(200);
  expect(f.audits).not.toHaveBeenCalled();
});
it("denies universe admins access to permanent deletion and deleted-session lists", async () => {
  const f = fixture();
  expect((await f.call("/admin/universes/universe/sessions/s1/purge")).status).toBe(403);
  expect((await f.call("/admin/universes/universe/deleted-sessions", "GET")).status).toBe(403);
  expect(f.rpcCalls).toEqual([]);
  expect(f.audits).not.toHaveBeenCalled();
});
it("allows platform admins to list and permanently delete, auditing the affected IDs", async () => {
  auth.platformAdmin = true;
  const f = fixture();
  expect((await f.call("/admin/universes/universe/deleted-sessions", "GET")).status).toBe(200);
  expect((await f.call("/admin/universes/universe/sessions/s1/purge")).status).toBe(200);
  expect(f.audits).toHaveBeenCalledWith(expect.objectContaining({ actorId: "member", universeId: "universe", targetId: "s1", action: "session.purge", outcome: "success", details: { deletedSessionIds: '["s1","child"]' } }));
});
it("does not duplicate purge audit when the runtime reports an already-removed session", async () => {
  auth.platformAdmin = true;
  const f = fixture({ purgedIds: [] });
  expect((await f.call("/admin/universes/universe/sessions/s1/purge")).status).toBe(200);
  expect(f.audits).not.toHaveBeenCalled();
});

it("audits permanent deletion after the universe soft-delete step exactly once", async () => {
  auth.platformAdmin = true;
  const f = fixture();
  expect((await f.call("/universes/universe/sessions/s1?cascade=true", "DELETE")).status).toBe(200);
  expect(f.audits).not.toHaveBeenCalled();
  expect((await f.call("/admin/universes/universe/sessions/s1/purge")).status).toBe(200);
  expect(f.rpcCalls).toEqual(["session/delete", "deployment/sessions/purge"]);
  expect(f.audits).toHaveBeenCalledOnce();
  expect(f.audits).toHaveBeenCalledWith(expect.objectContaining({
    actorId: "member", universeId: "universe", targetId: "s1", action: "session.purge",
    details: { deletedSessionIds: '["s1","child"]' }, outcome: "success",
  }));
});
