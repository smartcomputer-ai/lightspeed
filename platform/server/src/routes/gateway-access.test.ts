import { Hono } from "hono";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { EffectiveAccess } from "@lightspeed-ai/agent-client";
import type { ApiVariables, AppContext } from "../context.js";
import { requestIdentity } from "../runtime-client.js";
import { gatewayRoutes } from "./gateway.js";

vi.mock("./universes.js", () => ({ universeForSession: vi.fn(async () => ({
  universe: { lightspeedUniverseId: "universe", gatewayUrl: "https://engine.example/rpc" },
  slug: "test", role: "viewer",
})) }));
afterEach(() => vi.unstubAllGlobals());
function app() {
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (_c, next) => {
    await requestIdentity.run({ principal: { id: "11111111-1111-4111-8111-111111111111" } } as EffectiveAccess, next);
  });
  app.route("/", gatewayRoutes({ env: { lightspeedApiUrl: "https://engine.example/rpc", lightspeedApiKey: "lsk_fixture" } } as AppContext));
  return app;
}
describe("action permission previews", () => {
  it("forwards as the logged-in actor and preserves target-specific core decisions", async () => {
    const preview = { actions: ["read"], resources: [{ resource: { kind: "session", id: "other" }, actions: ["read"] }] };
    const fetch = vi.fn(async (_url: unknown, init: RequestInit) => {
      expect(new Headers(init.headers).get("x-lightspeed-principal")).toBe("user:11111111-1111-4111-8111-111111111111");
      const rpc = JSON.parse(String(init.body));
      expect(rpc.method).toBe("access/read");
      expect(rpc.params).toEqual({ resources: [{ kind: "session", id: "other" }], sessionDeleteCascade: true });
      return Response.json({ id: rpc.id, result: { result: preview, notifications: [] } });
    });
    vi.stubGlobal("fetch", fetch);
    const response = await app().request("/universe/access", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ resources: [{ kind: "session", id: "other" }], sessionDeleteCascade: true }) });
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual(preview);
  });
  it.each([
    { principalId: "another-user" },
    { resources: [{ kind: "session", id: "" }] },
    { resources: Array.from({ length: 101 }, () => ({ kind: "session", id: "one" })) },
  ])("rejects actor overrides and malformed or unbounded targets", async (body) => {
    const fetch = vi.fn();
    vi.stubGlobal("fetch", fetch);
    const response = await app().request("/universe/access", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    expect(response.status).toBe(400);
    expect(fetch).not.toHaveBeenCalled();
  });
});
