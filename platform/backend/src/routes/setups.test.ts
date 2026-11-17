import { userSubject } from "../../test/subject-fixture.js";
import { Hono } from "hono";
import { afterEach, expect, it, vi } from "vitest";
import type { UniverseRole } from "@lightspeed-ai/platform-shared";
import type { ApiVariables, AppContext } from "../context.js";
import { CONFIGURATOR_OPERATIONS, setupRoutes } from "./setups.js";
import { universeForRequest } from "./universes.js";

vi.mock("./universes.js", () => ({ universeForRequest: vi.fn() }));
afterEach(() => { vi.unstubAllGlobals(); vi.clearAllMocks(); });

/// The setup routes for a member holding `role`; the universe has no
/// installation yet, and nothing reaches the runtime.
function routes(role: UniverseRole) {
  const universe = { id: "platform-universe", lightspeedUniverseId: "33333333-3333-4333-8333-333333333333" };
  vi.mocked(universeForRequest).mockResolvedValue({
    universe, handle: "test", role, caller: { actorId: "platform-user", role },
  } as unknown as Awaited<ReturnType<typeof universeForRequest>>);
  const runtime = vi.fn();
  vi.stubGlobal("fetch", runtime);
  const writes = vi.fn();
  const ctx = {
    db: {
      select: () => ({ from: () => ({ where: async () => [] }) }),
      insert: writes,
      update: writes,
    },
    env: { lightspeedApiUrl: "https://runtime.example/api/v1", lightspeedApiToken: "lsr_fixture", configuratorMcpUrl: "https://configurator.example/mcp" },
  } as unknown as AppContext;
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    c.set("session", { user: { id: "platform-user" } } as ApiVariables["session"]);
    c.set("auth", userSubject(c.get("session").user.id, c.get("session").user.role === "admin"));
    await next();
  });
  app.route("/", setupRoutes(ctx));
  return { app, writes, runtime };
}

it.each(["operator", "owner"] as const)("lists Configurator as available for %s", async (role) => {
  const { app, writes, runtime } = routes(role);
  const response = await app.request("/platform-universe/setups");
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject([{ id: "configurator", status: "available", available: true }]);
  expect(writes).not.toHaveBeenCalled();
  expect(runtime).not.toHaveBeenCalled();
});
it.each(["viewer", "contributor"] as const)("hides setup from %s", async (role) => {
  const { app } = routes(role);
  expect((await app.request("/platform-universe/setups")).status).toBe(404);
});
it.each(["viewer", "contributor", "operator"] as const)("requires Owner for installation by %s", async (role) => {
  const { app, writes, runtime } = routes(role);
  const response = await app.request("/platform-universe/setups/configurator/install", { method: "POST" });
  expect(response.status).toBe(403);
  expect(writes).not.toHaveBeenCalled();
  expect(runtime).not.toHaveBeenCalled();
});

it("permits reviewed configuration operations without execution, lifecycle or identity administration", () => {
  expect(CONFIGURATOR_OPERATIONS).toContain("profiles/put");
  for (const operation of ["session/start", "session/runs/start", "credentials/grants/import", "credentials/grants/lease", "deployment/universes/create", "platform/organizations/service-users/create"]) expect(CONFIGURATOR_OPERATIONS).not.toContain(operation);
});
