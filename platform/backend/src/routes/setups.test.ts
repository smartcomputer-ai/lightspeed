import { Hono } from "hono";
import { afterEach, expect, it, vi } from "vitest";
import type { UniverseRole } from "@lightspeed-ai/platform-shared";
import type { ApiVariables, AppContext } from "../context.js";
import { setupRoutes } from "./setups.js";
import { universeForSession } from "./universes.js";

vi.mock("./universes.js", () => ({ universeForSession: vi.fn() }));
afterEach(() => { vi.unstubAllGlobals(); vi.clearAllMocks(); });

/// The setup routes for a member holding `role`; the universe has no
/// installation yet, and nothing reaches the runtime.
function routes(role: UniverseRole) {
  const universe = { id: "platform-universe", lightspeedUniverseId: "33333333-3333-4333-8333-333333333333", gatewayUrl: null };
  vi.mocked(universeForSession).mockResolvedValue({
    universe, handle: "test", role, member: { userId: "platform-user", role },
  } as unknown as Awaited<ReturnType<typeof universeForSession>>);
  const runtime = vi.fn();
  vi.stubGlobal("fetch", runtime);
  const writes = vi.fn();
  const ctx = {
    db: {
      select: () => ({ from: () => ({ where: () => ({ limit: async () => [] }) }) }),
      insert: writes,
      update: writes,
    },
    env: { lightspeedApiUrl: "https://runtime.example/rpc", lightspeedRpcToken: "lsr_fixture", configuratorMcpUrl: "https://configurator.example/mcp" },
  } as unknown as AppContext;
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    c.set("session", { user: { id: "platform-user" } } as ApiVariables["session"]);
    await next();
  });
  app.route("/", setupRoutes(ctx));
  return { app, writes, runtime };
}

it.each(["operator", "admin"] as const)("lists Configurator as unavailable for %s", async (role) => {
  const { app, writes, runtime } = routes(role);
  const response = await app.request("/platform-universe/setups");
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject([{ id: "configurator", status: "unavailable", available: false }]);
  expect(writes).not.toHaveBeenCalled();
  expect(runtime).not.toHaveBeenCalled();
});
it.each(["viewer", "contributor"] as const)("hides setup from %s", async (role) => {
  const { app } = routes(role);
  expect((await app.request("/platform-universe/setups")).status).toBe(404);
});
it.each(["viewer", "contributor", "operator", "admin"] as const)("never installs an agent credential for %s", async (role) => {
  const { app, writes, runtime } = routes(role);
  const response = await app.request("/platform-universe/setups/configurator/install", { method: "POST" });
  expect(response.status).toBe(role === "admin" ? 503 : 403);
  expect(writes).not.toHaveBeenCalled();
  expect(runtime).not.toHaveBeenCalled();
});
