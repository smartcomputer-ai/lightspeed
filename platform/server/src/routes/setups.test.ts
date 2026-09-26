import { Hono } from "hono";
import { afterEach, expect, it, vi } from "vitest";
import { LightspeedRpcError, type LightspeedClient, type MethodGroup } from "@lightspeed-ai/agent-client";
import type { UniverseRole } from "@lightspeed/platform-shared";
import type { ApiVariables, AppContext } from "../context.js";
import { ensureCredential, ensureMcpServer, setupRoutes, type KeyChoice } from "./setups.js";
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

it("requires a key choice before installing", async () => {
  const { app, writes, runtime } = routes("admin");
  for (const body of [{}, { key: { kind: "new", groups: [] } }, { key: { kind: "existing", keyPrefix: "lsk_x", secret: "nope" } }]) {
    const response = await app.request("/platform-universe/setups/configurator/install", {
      method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body),
    });
    expect(response.status).toBe(400);
  }
  expect(writes).not.toHaveBeenCalled();
  expect(runtime).not.toHaveBeenCalled();
});

/// The setup's credential step over a core holding the setup's live key with
/// `keyGroups` and its active grant, plus an admin's key `lsk_admin` whose
/// secret is `lsk_admin_secret`.
function credential(keyGroups: string[], keySource?: "minted" | "existing") {
  const mcpUrl = "https://configurator.example/mcp";
  const deploymentCalls: { method: string; params: Record<string, unknown> }[] = [];
  const deployment = {
    call: vi.fn(async (method: string, params: Record<string, unknown>) => {
      deploymentCalls.push({ method, params });
      if (method === "deployment/api-keys/list") {
        return { result: { apiKeys: [
          { keyPrefix: "lsk_old", displayName: "Lightspeed Configurator service credential", groups: keyGroups, revokedAtMs: null },
          { keyPrefix: "lsk_admin", displayName: "Admin's key", groups: ["session", "profiles"], revokedAtMs: null },
        ] } };
      }
      if (method === "deployment/api-keys/create") {
        return { result: { apiKey: { keyPrefix: "lsk_new" }, secret: "lsk_new_secret" } };
      }
      return { result: {} };
    }),
  } as unknown as LightspeedClient;
  const clientCalls: string[] = [];
  const client = {
    call: vi.fn(async (method: string) => {
      clientCalls.push(method);
      return method === "auth/grants/read"
        ? { result: { grant: { status: "active", audience: mcpUrl } } }
        : { result: {} };
    }),
  } as unknown as LightspeedClient;
  const ctx = {
    db: { update: () => ({ set: () => ({ where: async () => undefined }) }) },
  } as unknown as AppContext;
  const verify = (secret: string) => ({
    call: vi.fn(async () => {
      if (secret !== "lsk_admin_secret") throw new LightspeedRpcError({ code: -32001, message: "unauthenticated" });
      return { result: { caller: { keyPrefix: "lsk_admin", groups: ["session", "profiles"] } } };
    }),
  }) as unknown as LightspeedClient;
  const state = { keyPrefix: "lsk_old", keyGroups, grantId: "authgrant_old", ...(keySource ? { keySource } : {}) };
  return {
    deploymentCalls,
    clientCalls,
    run: (choice: KeyChoice) => ensureCredential(
      ctx, "installation", { lightspeedUniverseId: "33333333-3333-4333-8333-333333333333" },
      client, deployment, verify, state, mcpUrl, choice,
    ),
  };
}

it("keeps the Configurator key while its groups match", async () => {
  const { deploymentCalls, clientCalls, run } = credential(["mcp", "profiles"]);
  expect(await run({ kind: "new", groups: ["profiles", "mcp"] })).toMatchObject({ keyPrefix: "lsk_old", grantId: "authgrant_old" });
  expect(deploymentCalls.map((call) => call.method)).toEqual(["deployment/api-keys/list"]);
  expect(clientCalls).toEqual(["auth/grants/read"]);
});

it("replaces the Configurator key and grant when its groups change", async () => {
  const { deploymentCalls, clientCalls, run } = credential(["mcp", "profiles"]);
  const state = await run({ kind: "new", groups: ["models", "profiles"] as MethodGroup[] });
  expect(state).toMatchObject({ keyPrefix: "lsk_new", keyGroups: ["models", "profiles"] });
  expect(state.grantId).not.toBe("authgrant_old");
  expect(deploymentCalls.map((call) => call.method)).toEqual([
    "deployment/api-keys/list", "deployment/api-keys/revoke", "deployment/api-keys/create",
  ]);
  expect(deploymentCalls[2]?.params).toMatchObject({ groups: ["models", "profiles"], assertActor: false });
  expect(clientCalls).toEqual(["auth/grants/read", "auth/grants/revoke", "auth/grants/import"]);
});

it("switches to an admin's existing key after checking its secret, revoking only what the setup minted", async () => {
  const { deploymentCalls, clientCalls, run } = credential(["mcp", "profiles"]);
  await expect(run({ kind: "existing", keyPrefix: "lsk_admin", secret: "lsk_wrong" })).rejects.toThrow(/does not belong/);
  await expect(run({ kind: "existing", keyPrefix: "lsk_gone", secret: "lsk_admin_secret" })).rejects.toThrow(/not an active key/);
  expect(clientCalls.filter((method) => method !== "auth/grants/read")).toEqual([]);

  const state = await run({ kind: "existing", keyPrefix: "lsk_admin", secret: "lsk_admin_secret" });
  expect(state).toMatchObject({ keyPrefix: "lsk_admin", keyGroups: ["session", "profiles"], keySource: "existing" });
  expect(deploymentCalls.filter((call) => call.method === "deployment/api-keys/revoke").map((call) => call.params))
    .toEqual([{ keyPrefix: "lsk_old" }]);
  expect(clientCalls.slice(-2)).toEqual(["auth/grants/revoke", "auth/grants/import"]);
});

it("never revokes an existing key it replaces, and keeps the current key on request", async () => {
  const brought = credential(["mcp", "profiles"], "existing");
  expect(await brought.run({ kind: "current" })).toMatchObject({ keyPrefix: "lsk_old" });
  await brought.run({ kind: "new", groups: ["profiles"] });
  expect(brought.deploymentCalls.map((call) => call.method)).toEqual([
    "deployment/api-keys/list", "deployment/api-keys/list", "deployment/api-keys/create",
  ]);
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
