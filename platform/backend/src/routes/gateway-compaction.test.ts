import { userSubject } from "../../test/subject-fixture.js";
import { decodeHttpRequest } from "../../../test/http-fixture.js";
import { Hono } from "hono";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ApiVariables, AppContext } from "../context.js";
import { gatewayRoutes } from "./gateway.js";

const access = vi.hoisted(() => ({ role: "contributor", owner: "user" }));
vi.mock("./universes.js", () => ({
  universeForSession: vi.fn(async () => ({
    universe: { lightspeedUniverseId: "universe", gatewayUrl: "https://engine.example/api/v1" },
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
    const request = decodeHttpRequest(_url as string | Request | URL, init);
    requests.push({ method: request.method, params: request.params });
    const session = { id: "session", lifecycleStatus: "open", activity: "idle", access: { visibility: "restricted", createdBy: { kind: "actor", id: access.owner } } };
    return Response.json({ session });
  }));
});
afterEach(() => vi.unstubAllGlobals());
async function compact() {
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    c.set("session", { user: { id: "user" } } as ApiVariables["session"]);
    c.set("auth", userSubject(c.get("session").user.id, c.get("session").user.role === "admin"));
    await next();
  });
  app.route("/", gatewayRoutes({ env: { lightspeedApiUrl: "https://engine.example/api/v1", lightspeedApiToken: "lsr_fixture" } } as AppContext));
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
