import { Hono } from "hono";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiVariables, AppContext } from "../context.js";
import { gatewayRoutes } from "./gateway.js";

vi.mock("./universes.js", () => ({
  universeForSession: vi.fn(async () => ({
    universe: { lightspeedUniverseId: "universe", gatewayUrl: "https://engine.example/rpc" },
    slug: "test",
    role: "owner",
  })),
}));

afterEach(() => vi.unstubAllGlobals());

describe("composer input origin", () => {
  it.each([
    ["/universe/sessions/session/messages", "session/runs/start", { submissionId: "submission" }],
    ["/universe/sessions/session/runs/run/steer", "session/runs/steer", {}],
  ])("attaches the authenticated user on %s", async (path, method, extra) => {
    const requests: Array<{ method: string; params: Record<string, unknown> }> = [];
    vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
      expect(new Headers(init.headers).get("authorization")).toBe("Bearer lsk_platform_fixture");
      expect(new Headers(init.headers).has("x-lightspeed-principal")).toBe(false);
      expect(init.redirect).toBe("error");
      const rpc = JSON.parse(String(init.body));
      requests.push(rpc);
      return Response.json({
        id: rpc.id,
        result: {
          result: { run: { id: "run", status: "running" }, steeringId: "steer" },
          notifications: [],
        },
      });
    }));
    const app = new Hono<{ Variables: ApiVariables }>();
    app.use("*", async (c, next) => {
      c.set("session", { user: { id: "operator" } } as ApiVariables["session"]);
      await next();
    });
    app.route("/", gatewayRoutes({ env: { lightspeedApiUrl: "https://engine.example/rpc", lightspeedApiKey: "lsk_platform_fixture" } } as AppContext));
    const response = await app.request(path, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ text: "hello", origin: "user:someone-else", ...extra }),
    });
    expect(response.status).toBe(200);
    expect(requests).toHaveLength(1);
    const request = requests[0];
    if (!request) throw new Error("expected a gateway request");
    expect(request.method).toBe(method);
    const params = request.params;
    const items = method === "session/runs/start" ? (params.source as { items: unknown[] }).items : params.items;
    expect(items).toEqual([{ type: "text", text: "hello", origin: "user:operator" }]);
  });
});
