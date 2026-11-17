import { userSubject } from "../../test/subject-fixture.js";
import { universeActions, type UniverseRole } from "@lightspeed-ai/platform-shared";
import { decodeHttpRequest, problemResponse } from "../../../test/http-fixture.js";
import { Hono } from "hono";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ApiVariables, AppContext } from "../context.js";
import { gatewayRoutes } from "./gateway.js";
import { sessionAdminRoutes } from "./session-admin.js";

const auth = vi.hoisted(() => ({ role: "owner", platformAdmin: false }));
vi.mock("./universes.js", () => ({ universeForRequest: vi.fn(async () => ({
  universe: { id: "universe", lightspeedUniverseId: "runtime-universe" },
  handle: "test", role: auth.role, actions: universeActions([auth.role as UniverseRole]), caller: { actorId: "member", actions: universeActions([auth.role as UniverseRole]) },
})) }));
beforeEach(() => { auth.role = "owner"; auth.platformAdmin = false; });
afterEach(() => vi.unstubAllGlobals());

function fixture({ failMutation = false, purgedIds = ["s1", "child"] }: { failMutation?: boolean; purgedIds?: string[] } = {}) {
  const audits = vi.fn(async () => undefined);
  const operationCalls: string[] = [];
  const operationParams: Record<string, unknown>[] = [];
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    const request = decodeHttpRequest(_url as string | Request | URL, init);
    operationCalls.push(request.method);
    operationParams.push(request.params);
    if (failMutation && request.method !== "session/read") {
      return problemResponse({ type: "about:blank", title: "failed", detail: "failed", status: 409, kind: "conflict" });
    }
    const result = request.method === "deployment/sessions/purge" ? { deletedSessionIds: purgedIds }
      : request.method === "deployment/sessions/deleted/list" ? { sessions: [], nextAfter: null }
      : { session: { id: "s1", access: { visibility: "universe" } }, deletedSessionCount: 2 };
    return Response.json(result);
  }));
  const db = { insert: () => ({ values: audits }), select: () => ({ from: () => ({ where: () => ({ limit: async () => [{ id: "universe", lightspeedUniverseId: "runtime-universe" }] }) }) }) };
  const ctx = { db, env: { lightspeedApiUrl: "https://engine.example/api/v1", lightspeedApiToken: "lsr_fixture" } } as unknown as AppContext;
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    c.set("session", { user: { id: "member", role: auth.platformAdmin ? "admin" : "user" } } as ApiVariables["session"]);
    c.set("auth", userSubject(c.get("session").user.id, c.get("session").user.role === "admin"));
    await next();
  });
  app.route("/universes", gatewayRoutes(ctx));
  app.route("/admin", sessionAdminRoutes(ctx));
  const call = (path: string, method = "POST", body: unknown = {}) => app.request(path, { method,
    headers: { "content-type": "application/json" }, ...(method === "POST" ? { body: JSON.stringify(body) } : {}),
  });
  return { call, audits, operationCalls, operationParams };
}

it.each([false, true])("closes with force=%s without writing an audit record", async (force) => {
  const f = fixture();
  expect((await f.call("/universes/universe/sessions/s1/close", "POST", { force })).status).toBe(200);
  expect(f.operationCalls).toContain("session/close");
  expect(f.audits).not.toHaveBeenCalled();
});
it.each(["operator", "owner"])("%s restarts a stopped session through the runtime", async (role) => {
  auth.role = role;
  const f = fixture();
  expect((await f.call("/universes/universe/sessions/s1/restart")).status).toBe(200);
  expect(f.operationParams[f.operationCalls.indexOf("session/restart")]).toEqual({ sessionId: "s1" });
});
it("does not let a contributor restart a session", async () => {
  auth.role = "contributor";
  const f = fixture();
  expect((await f.call("/universes/universe/sessions/s1/restart")).status).toBe(403);
  expect(f.operationCalls).not.toContain("session/restart");
});
it.each(["operator", "owner"])("%s always soft deletes even if the URL requests permanent deletion", async (role) => {
  auth.role = role;
  const f = fixture();
  expect((await f.call("/universes/universe/sessions/s1?cascade=true&softDelete=false", "DELETE")).status).toBe(200);
  expect(f.operationCalls).toContain("session/delete");
  expect(f.operationParams[f.operationCalls.indexOf("session/delete")]).toMatchObject({ sessionId: "s1", cascade: true, softDelete: true });
  expect(f.audits).not.toHaveBeenCalled();
});
it("does not record successful purge when the runtime rejects it", async () => {
  auth.platformAdmin = true;
  const f = fixture({ failMutation: true });
  expect((await f.call("/admin/universes/universe/sessions/s1/purge")).status).not.toBe(200);
  expect(f.audits).not.toHaveBeenCalled();
});
it("denies universe Owners access to permanent deletion and deleted-session lists", async () => {
  const f = fixture();
  expect((await f.call("/admin/universes/universe/sessions/s1/purge")).status).toBe(403);
  expect((await f.call("/admin/universes/universe/deleted-sessions", "GET")).status).toBe(403);
  expect(f.operationCalls).toEqual([]);
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
  expect(f.operationCalls).toEqual(["session/delete", "deployment/sessions/purge"]);
  expect(f.audits).toHaveBeenCalledOnce();
  expect(f.audits).toHaveBeenCalledWith(expect.objectContaining({
    actorId: "member", universeId: "universe", targetId: "s1", action: "session.purge",
    details: { deletedSessionIds: '["s1","child"]' }, outcome: "success",
  }));
});
