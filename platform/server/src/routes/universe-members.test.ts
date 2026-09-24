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
  const capability = (memberId: string, enabled: boolean) => app.request(`/platform-universe/members/${memberId}/private-content-access`, {
    method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ enabled }),
  });
  return { edit, changes, capability };
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
  [`principal:${subjectId}:executor`, "viewer"],
  [`principal:${subjectId}:viewer`, "executor"],
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

it.each([true, false])("changes private-content access through one attributed capability operation (%s)", async (enabled) => {
  const { capability, changes } = setup();
  const response = await capability(`principal:${subjectId}:viewer`, enabled);
  expect(response.status).toBe(200);
  expect(await response.json()).toEqual({ enabled });
  expect(changes).toEqual([{ operation: enabled ? "assign_capability" : "revoke_capability", assignment: {
    scope: { kind: "universe", universeId }, principalId: subjectId, capability: "read_private_content",
  } }]);
});
it.each(["viewer", "contributor", "operator"])("does not let a %s assign private-content access", async (role) => {
  const { capability, changes } = setup(role);
  expect((await capability(`principal:${subjectId}:viewer`, true)).status).toBe(403);
  expect(changes).toEqual([]);
});
it("refuses group capability targets and propagates core refusal", async () => {
  const group = setup();
  expect((await group.capability(`group:${subjectId}:viewer`, true)).status).toBe(400);
  expect(group.changes).toEqual([]);
  const denied = setup("admin", true);
  expect((await denied.capability(`principal:${subjectId}:viewer`, true)).status).toBe(409);
  expect(denied.changes).toHaveLength(1);
});

it("lists the execution identity as a read-only system member", async () => {
  const executor = "44444444-4444-4444-8444-444444444444";
  const scope = { kind: "universe", universeId };
  const rights = { principal: { id: actorId }, roles: ["admin"] } as EffectiveAccess;
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
    if (rpc.method === "deployment/identity/self") return Response.json({ id: rpc.id, result: { result: { access: rights, universes: [] }, notifications: [] } });
    expect(rpc.method).toBe("deployment/identity/directory");
    return Response.json({ id: rpc.id, result: { result: {
      principals: [
        { id: subjectId, kind: "user", status: "active", displayName: "Alice", managementScope: { kind: "deployment" }, createdAtMs: 0 },
        { id: executor, kind: "service", status: "active", displayName: "Default agent identity", managementScope: scope, createdAtMs: 0 },
      ],
      groups: [], memberships: [], capabilities: [], policyRevision: 1,
      roles: [
        { scope, subject: { kind: "principal", id: subjectId }, role: "contributor" },
        { scope, subject: { kind: "principal", id: executor }, role: "executor" },
      ],
    }, notifications: [] } });
  }));
  const universe = { id: "platform-universe", lightspeedUniverseId: universeId, slug: "test", gatewayUrl: null };
  // `select().from(universes).where().limit()` and `await select().from(user)`.
  const from = () => Object.assign(Promise.resolve([]), { where: () => ({ limit: async () => [universe] }) });
  const ctx = { db: { select: () => ({ from }) }, env: { lightspeedApiUrl: "https://runtime.example/rpc", lightspeedApiKey: "lsk_fixture" } } as unknown as AppContext;
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    c.set("session", { user: { id: "platform-user" } } as ApiVariables["session"]);
    await requestIdentity.run(rights, next);
  });
  app.route("/", universeRoutes(ctx));
  const response = await app.request("/platform-universe/members");
  expect(response.status).toBe(200);
  const members = await response.json() as { subject: { id: string }; role: string; system: boolean; name: string; email: string }[];
  expect(members.map((member) => [member.subject.id, member.role, member.system])).toEqual([
    [subjectId, "contributor", false],
    [executor, "executor", true],
  ]);
  expect(members[1]).toMatchObject({ name: "Default agent identity", email: "Agent identity" });
});
