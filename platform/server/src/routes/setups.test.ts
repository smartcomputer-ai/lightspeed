import { Hono } from "hono";
import { afterEach, expect, it, vi } from "vitest";
import { LightspeedRpcError, type EffectiveAccess, type LightspeedClient } from "@lightspeed-ai/agent-client";
import { schema } from "@lightspeed/platform-db";
import type { ApiVariables, AppContext } from "../context.js";
import { requestIdentity } from "../runtime-client.js";
import { ensureMcpServer, setupRoutes } from "./setups.js";

afterEach(() => vi.unstubAllGlobals());

/// The setup routes for a caller holding `role`; the universe has no
/// installation yet, and only the identity lookup reaches the runtime.
function routes(role: string) {
  const rights = { principal: { id: "11111111-1111-4111-8111-111111111111" }, roles: [role] } as EffectiveAccess;
  const runtime = vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
    expect(rpc.method).toBe("deployment/identity/self");
    return Response.json({ id: rpc.id, result: { result: { access: rights, universes: [] }, notifications: [] } });
  });
  vi.stubGlobal("fetch", runtime);
  const universe = { id: "platform-universe", lightspeedUniverseId: "33333333-3333-4333-8333-333333333333", slug: "test", gatewayUrl: null };
  const writes = vi.fn();
  const ctx = {
    db: {
      select: () => ({ from: (table: unknown) => ({ where: () => ({ limit: async () => (table === schema.universes ? [universe] : []) }) }) }),
      insert: writes,
      update: writes,
    },
    env: { lightspeedApiUrl: "https://runtime.example/rpc", lightspeedApiKey: "lsk_fixture", configuratorMcpUrl: "https://configurator.example/mcp" },
  } as unknown as AppContext;
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    c.set("session", { user: { id: "platform-user" } } as ApiVariables["session"]);
    await requestIdentity.run(rights, next);
  });
  app.route("/", setupRoutes(ctx));
  return { app, writes };
}

it.each(["operator", "admin"])("lists templates for a universe %s", async (role) => {
  const { app } = routes(role);
  const response = await app.request("/platform-universe/setups");
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject([{ id: "configurator", status: "available", available: true }]);
});

it.each(["viewer", "contributor"])("hides templates from a universe %s", async (role) => {
  const { app } = routes(role);
  expect((await app.request("/platform-universe/setups")).status).toBe(404);
});

it.each(["viewer", "contributor", "operator"])("keeps installing with Admins, refusing a %s", async (role) => {
  const { app, writes } = routes(role);
  const response = await app.request("/platform-universe/setups/configurator/install", { method: "POST" });
  expect(response.status).toBe(403);
  expect(writes).not.toHaveBeenCalled();
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

it("registers a new Configurator server restricted to the installing Admin", async () => {
  const { puts, run } = install(null);
  await run();
  expect(puts).toHaveLength(1);
  expect(puts[0]).toMatchObject({ access: { visibility: "restricted" } });
  expect(puts[0]).not.toHaveProperty("expectedRevision");
});

it("repairs an installed server without touching the access chosen since", async () => {
  const { puts, run } = install({ revision: 4 });
  await run();
  expect(puts).toHaveLength(1);
  expect(puts[0]).toMatchObject({ expectedRevision: 4 });
  expect(puts[0]).not.toHaveProperty("access");
});
