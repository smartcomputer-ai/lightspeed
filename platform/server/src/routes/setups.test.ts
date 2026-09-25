import { Hono } from "hono";
import { afterEach, expect, it, vi } from "vitest";
import { LightspeedRpcError, type LightspeedClient } from "@lightspeed-ai/agent-client";
import type { UniverseRole } from "@lightspeed/platform-shared";
import type { ApiVariables, AppContext } from "../context.js";
import { ensureMcpServer, setupRoutes } from "./setups.js";
import { universeForSession } from "./universes.js";

vi.mock("./universes.js", () => ({ universeForSession: vi.fn() }));
afterEach(() => { vi.unstubAllGlobals(); vi.clearAllMocks(); });

/// The setup routes for a member holding `role`; the universe has no
/// installation yet, and nothing reaches the runtime.
function routes(role: UniverseRole) {
  const universe = { id: "platform-universe", lightspeedUniverseId: "33333333-3333-4333-8333-333333333333", gatewayUrl: null };
  vi.mocked(universeForSession).mockResolvedValue({
    universe, slug: "test", role, member: { userId: "platform-user", role },
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
    env: { lightspeedApiUrl: "https://runtime.example/rpc", lightspeedApiKey: "lsk_fixture", configuratorMcpUrl: "https://configurator.example/mcp" },
  } as unknown as AppContext;
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    c.set("session", { user: { id: "platform-user" } } as ApiVariables["session"]);
    await next();
  });
  app.route("/", setupRoutes(ctx));
  return { app, writes, runtime };
}

it.each(["operator", "admin"] as const)("lists templates for a universe %s", async (role) => {
  const { app } = routes(role);
  const response = await app.request("/platform-universe/setups");
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject([{ id: "configurator", status: "available", available: true }]);
});

it.each(["viewer", "contributor"] as const)("hides templates from a universe %s", async (role) => {
  const { app } = routes(role);
  expect((await app.request("/platform-universe/setups")).status).toBe(404);
});

it.each(["viewer", "contributor", "operator"] as const)("keeps installing with Admins, refusing a %s", async (role) => {
  const { app, writes, runtime } = routes(role);
  const response = await app.request("/platform-universe/setups/configurator/install", { method: "POST" });
  expect(response.status).toBe(403);
  expect(writes).not.toHaveBeenCalled();
  expect(runtime).not.toHaveBeenCalled();
});

function install(existing: { revision: number } | null) {
  const puts: Record<string, unknown>[] = [];
  const client = {
    call: vi.fn(async (method: string, params: Record<string, unknown>) => {
      if (method === "mcp/servers/read") {
        if (!existing) throw new LightspeedRpcError({ code: -32004, message: "not found" });
        return { result: { server: { serverId: "lightspeed-configurator", ...existing } } };
      }
      expect(method).toBe("mcp/servers/put");
      puts.push(params);
      return { result: { server: params.server } };
    }),
  } as unknown as LightspeedClient;
  const ctx = {
    db: { update: () => ({ set: () => ({ where: async () => undefined }) }) },
  } as unknown as AppContext;
  const state = {
    grantId: "authgrant_configurator",
    ...(existing ? { serverId: "lightspeed-configurator" } : {}),
  };
  return {
    puts,
    run: () => ensureMcpServer(ctx, "installation", client, state, "https://configurator.example/mcp", false),
  };
}

it("registers a new Configurator server", async () => {
  const { puts, run } = install(null);
  await run();
  expect(puts).toHaveLength(1);
  expect(puts[0]).toMatchObject({ server: { serverId: "lightspeed-configurator" } });
  expect(puts[0]).not.toHaveProperty("expectedRevision");
});

it("repairs an installed server at its current revision", async () => {
  const { puts, run } = install({ revision: 4 });
  await run();
  expect(puts).toHaveLength(1);
  expect(puts[0]).toMatchObject({ expectedRevision: 4 });
});
