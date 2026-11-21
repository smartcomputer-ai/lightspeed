import { Hono } from "hono";
import { expect, it, vi } from "vitest";
import type { UniverseModuleEnv } from "@lightspeed-ai/platform-backend/modules";
import sessionCollectionRoutes from "./index.js";

function moduleApp() {
  const call = vi.fn();
  const app = new Hono<UniverseModuleEnv>();
  app.use("*", async (c, next) => {
    c.set("client", { call });
    await next();
  });
  app.route("/modules/sessions", sessionCollectionRoutes());
  return { app, call };
}

it("translates browser filters and pages the admitted client's results", async () => {
  const { app, call } = moduleApp();
  call.mockResolvedValue({ sessions: [{ id: "session-a" }], nextCursor: "b" });
  const response = await app.request("/modules/sessions?closed=false&metadata=team%3Dops&cursor=a&limit=500");
  expect(response.status).toBe(200);
  expect(await response.json()).toEqual({ sessions: [{ id: "session-a" }], nextCursor: "b" });
  expect(call).toHaveBeenCalledWith("session/list", expect.objectContaining({
    closed: false, metadata: { team: "ops" }, cursor: "a", limit: 200,
  }));
});

it("starts work from a prepared profile and returns the new conversation", async () => {
  const { app, call } = moduleApp();
  call.mockResolvedValueOnce({ session: { id: "session-a" } })
    .mockResolvedValueOnce({ session: { id: "session-a", displayName: "Review" } });
  const input = { profile: { kind: "named", profileId: "reviewer" }, displayName: "Review" };
  const response = await app.request("/modules/sessions", { method: "POST", body: JSON.stringify(input) });
  expect(response.status).toBe(200);
  expect(await response.json()).toEqual({ id: "session-a", displayName: "Review" });
  expect(call.mock.calls).toEqual([["session/start", input], ["session/read", { sessionId: "session-a" }]]);
});
