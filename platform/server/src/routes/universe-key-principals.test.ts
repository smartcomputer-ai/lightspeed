import { Hono } from "hono";
import { afterEach, expect, it, vi } from "vitest";
import type { EffectiveAccess, IdentityPrincipal } from "@lightspeed-ai/agent-client";
import type { ApiVariables, AppContext } from "../context.js";
import { requestIdentity } from "../runtime-client.js";
import { universeRoutes } from "./universes.js";

const scope = { kind: "universe" as const, universeId: "11111111-1111-4111-8111-111111111111" };
const me = { id: "22222222-2222-4222-8222-222222222222", kind: "user", status: "active", displayName: "Me", managementScope: { kind: "deployment" }, createdAtMs: 0 } as IdentityPrincipal;
afterEach(() => vi.unstubAllGlobals());
it.each(["viewer", "admin"])("offers only issuable principal choices to a universe %s", async (role) => {
  const calls: string[] = [];
  const rights = { principal: me, scope, roles: [role], capabilities: [], policyRevision: 1 } as EffectiveAccess;
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    expect(new Headers(init.headers).get("x-lightspeed-principal")).toBe(`user:${me.id}`);
    const rpc = JSON.parse(String(init.body));
    calls.push(rpc.method);
    const result = rpc.method === "deployment/identity/self" ? { access: rights, universes: [] } : {
      principals: [me,
        { ...me, id: "service", kind: "service", displayName: "Universe bot", managementScope: scope },
        { ...me, id: "executor", kind: "service", displayName: "Default agent identity", managementScope: scope },
        { ...me, id: "disabled", kind: "service", status: "disabled", managementScope: scope },
        { ...me, id: "deployment-service", kind: "service" },
        { ...me, id: "foreign-service", kind: "service", managementScope: { kind: "universe", universeId: "other" } },
        { ...me, id: "another-user" },
      ], groups: [], memberships: [], capabilities: [], policyRevision: 1,
      // An execution identity never gets a key.
      roles: [{ scope, subject: { kind: "principal", id: "executor" }, role: "executor" }],
    };
    return Response.json({ id: rpc.id, result: { result, notifications: [] } });
  }));
  const universe = { id: "platform-universe", lightspeedUniverseId: scope.universeId, slug: "test", gatewayUrl: null };
  const query = { from: () => query, where: () => query, limit: async () => [universe] };
  const ctx = { db: { select: () => query }, env: { lightspeedApiUrl: "https://runtime.example/rpc", lightspeedApiKey: "lsk_fixture" } } as unknown as AppContext;
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    c.set("session", { user: { id: "platform-user" } } as ApiVariables["session"]);
    await requestIdentity.run(rights, next);
  });
  app.route("/", universeRoutes(ctx));
  const response = await app.request("/platform-universe/key-principals");
  expect(response.status).toBe(200);
  expect(await response.json()).toEqual([
    { id: me.id, displayName: "Me", kind: "user" },
    ...(role === "admin" ? [{ id: "service", displayName: "Universe bot", kind: "service" }] : []),
  ]);
  expect(calls.includes("deployment/identity/directory")).toBe(role === "admin");
});
