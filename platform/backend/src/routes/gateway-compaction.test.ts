import { Hono } from "hono";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ApiVariables, AppContext } from "../context.js";
import { gatewayRoutes } from "./gateway.js";

const access = vi.hoisted(() => ({ role: "contributor", owner: "user" }));
vi.mock("./universes.js", () => ({
  universeForSession: vi.fn(async () => ({
    universe: { lightspeedUniverseId: "universe", gatewayUrl: "https://engine.example/rpc" },
    role: access.role,
    member: { userId: "user", role: access.role },
  })),
}));
let requests: Array<{ method: string; params: unknown }>;
beforeEach(() => {
  access.role = "contributor";
  access.owner = "user";
  requests = [];
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
    requests.push(rpc);
    const session = { id: "session", status: "idle", access: { visibility: "restricted", createdBy: { kind: "actor", id: access.owner } } };
    return Response.json({ id: rpc.id, result: { result: { session }, notifications: [] } });
  }));
});
afterEach(() => vi.unstubAllGlobals());
async function compact() {
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    c.set("session", { user: { id: "user" } } as ApiVariables["session"]);
    await next();
  });
  app.route("/", gatewayRoutes({ env: { lightspeedApiUrl: "https://engine.example/rpc", lightspeedApiKey: "lsk_fixture" } } as AppContext));
  return app.request("/universe/sessions/session/context/compact", { method: "POST" });
}
it("forwards a contributor's manual compaction to the core API", async () => {
  const response = await compact();
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject({ session: { id: "session" } });
  expect(requests).toContainEqual(expect.objectContaining({ method: "session/context/compact", params: { sessionId: "session" } }));
});
it("rejects viewers before requesting compaction", async () => {
  access.role = "viewer";
  expect((await compact()).status).toBe(403);
  expect(requests.some((request) => request.method === "session/context/compact")).toBe(false);
});
it("does not compact another user's private session", async () => {
  access.owner = "someone-else";
  expect((await compact()).status).toBe(404);
  expect(requests.some((request) => request.method === "session/context/compact")).toBe(false);
});
