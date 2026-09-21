import { Hono } from "hono";
import { afterEach, expect, it, vi } from "vitest";
import type { EffectiveAccess } from "@lightspeed-ai/agent-client";
import type { ApiVariables, AppContext } from "../context.js";
import { requestIdentity } from "../runtime-client.js";
import { universeRoutes } from "./universes.js";

const actorId = "11111111-1111-4111-8111-111111111111";
const subjectId = "22222222-2222-4222-8222-222222222222";
const universeId = "33333333-3333-4333-8333-333333333333";
afterEach(() => vi.unstubAllGlobals());

function setup(role = "admin", failure = false) {
  const changes: unknown[] = [];
  const rights = { principal: { id: actorId }, roles: [role] } as EffectiveAccess;
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    expect(new Headers(init.headers).get("x-lightspeed-principal")).toBe(`user:${actorId}`);
    const rpc = JSON.parse(String(init.body));
    if (rpc.method === "deployment/identity/self") return Response.json({ id: rpc.id, result: { result: { access: rights, universes: [] }, notifications: [] } });
    expect(rpc.method).toBe("deployment/identity/apply");
    changes.push(rpc.params);
    if (failure) return Response.json({ id: rpc.id, error: { code: -32009, message: "last administrator", data: { kind: "conflict", message: "last administrator" } } });
    return Response.json({ id: rpc.id, result: { result: { changed: true, policyRevision: 2 }, notifications: [] } });
  }));
  const universe = { id: "platform-universe", lightspeedUniverseId: universeId, slug: "test", gatewayUrl: null };
  const query = { from: () => query, where: () => query, limit: async () => [universe] };
  const ctx = { db: { select: () => query }, env: { lightspeedApiUrl: "https://runtime.example/rpc", lightspeedApiKey: "lsk_fixture" } } as unknown as AppContext;
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    c.set("session", { user: { id: "platform-user" } } as ApiVariables["session"]);
    await requestIdentity.run(rights, next);
  });
  app.route("/", universeRoutes(ctx));
  const edit = (memberId: string, role: string) => app.request(`/platform-universe/members/${memberId}`, {
    method: "PATCH", headers: { "content-type": "application/json" }, body: JSON.stringify({ role }),
  });
  return { edit, changes };
}

it.each(["principal", "group"])("replaces a %s grant in one attributed core operation", async (kind) => {
  const { edit, changes } = setup();
  const response = await edit(`${kind}:${subjectId}:viewer`, "operator");
  expect(response.status).toBe(200);
  expect(changes).toEqual([{ operation: "replace_role", assignment: {
    scope: { kind: "universe", universeId }, subject: { kind, id: subjectId }, role: "viewer",
  }, role: "operator" }]);
});

it.each(["viewer", "contributor", "operator"])("rejects edits by a universe %s", async (role) => {
  const { edit, changes } = setup(role);
  expect((await edit(`principal:${subjectId}:viewer`, "admin")).status).toBe(403);
  expect(changes).toEqual([]);
});

it.each([
  [`principal:${subjectId}:viewer`, "deployment_admin"],
  [`principal:${subjectId}:deployment_admin`, "admin"],
  [`principal:${subjectId}:viewer:extra`, "admin"],
  ["principal:missing:viewer", "admin"],
])("rejects invalid member edits %s → %s", async (id, role) => {
  const { edit, changes } = setup();
  expect((await edit(id, role)).status).toBe(400);
  expect(changes).toEqual([]);
});

it("returns the core conflict without attempting a fallback grant", async () => {
  const { edit, changes } = setup("admin", true);
  expect((await edit(`principal:${subjectId}:admin`, "viewer")).status).toBe(409);
  expect(changes).toHaveLength(1);
});
