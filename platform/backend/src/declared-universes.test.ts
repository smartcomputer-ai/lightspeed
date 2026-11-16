import { afterAll, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { Hono } from "hono";
import { universeRoutes } from "./routes/universes.js";
import type { ApiVariables } from "./context.js";
import { eq } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { identityEnv, identityFixture } from "../test/identity-fixture.js";
import { effectiveDeclaredRole, syncDeclaredUniverses, type UniverseConfiguration } from "./declared-universes.js";
import type { AppContext } from "./context.js";

const issuer = "https://identity.example.test";
const identity = { email: "owner@example.test", emailVerified: true, oidcIssuer: issuer, oidcSubject: "owner", identitySource: "company", companyAdmitted: true, directoryGroups: ["research-readers"] };
it("combines independent manual, verified-email, subject and exact issuer-qualified group grants", () => {
  const policy: UniverseConfiguration = {
    members: [{ email: "OWNER@example.test", role: "contributor" }, { issuer, subject: "owner", role: "operator" }],
    directoryGroupMappings: [{ issuer, group: "research-readers", role: "viewer" }],
  };
  expect(effectiveDeclaredRole(policy, identity)).toBe("operator");
  expect(effectiveDeclaredRole(policy, identity, "admin")).toBe("admin");
  expect(effectiveDeclaredRole({}, identity, "contributor")).toBe("contributor");
  expect(effectiveDeclaredRole({}, identity)).toBeUndefined();
  expect(effectiveDeclaredRole({ members: policy.members!.slice(0, 1) }, { ...identity, emailVerified: false })).toBeUndefined();
  expect(effectiveDeclaredRole(policy, { ...identity, emailVerified: false, oidcIssuer: "https://other.example" })).toBeUndefined();
  expect(effectiveDeclaredRole({ directoryGroupMappings: policy.directoryGroupMappings }, { ...identity, directoryGroups: ["research-readers-extra"] })).toBeUndefined();
  expect(effectiveDeclaredRole(policy, { ...identity, emailVerified: false, companyAdmitted: false })).toBeUndefined();
  expect(effectiveDeclaredRole(policy, { ...identity, banned: true }, "admin")).toBeUndefined();
});

let fixture: Awaited<ReturnType<typeof identityFixture>>;
let context: AppContext;
let inventory: Record<string, unknown>[];
let absentKind: "gone" | "notFound";
const first = "11111111-1111-4111-8111-111111111111";
const second = "22222222-2222-4222-8222-222222222222";
beforeAll(async () => {
  fixture = await identityFixture({ ...identityEnv, lightspeedApiUrl: "https://runtime.example/rpc", lightspeedApiKey: "lsk_fixture" });
  context = { ...fixture, pool: {} as never };
}, 30000);
afterAll(async () => { await fixture?.postgres.close(); vi.unstubAllGlobals(); });
beforeEach(async () => {
  await fixture.postgres.exec('DELETE FROM "organization"');
  absentKind = "notFound";
  inventory = [{ universeId: first, handle: "research", displayName: "Research", members: [], directoryGroupMappings: [{ issuer, group: "research-readers", role: "viewer" }] }, { universeId: second, handle: "unnamed" }];
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
    if (rpc.method === "deployment/universes/read") return Response.json({ id: rpc.id, error: { code: -32004, message: absentKind, data: { kind: absentKind, message: absentKind } } });
    expect(rpc.method).toBe("deployment/universes/list");
    return Response.json({ id: rpc.id, result: { result: { universes: inventory }, notifications: [] } });
  }));
});
it("adopts only named universes without an invented owner and is idempotent", async () => {
  expect(await syncDeclaredUniverses(context)).toEqual({ adopted: 1, updated: 0 });
  const [row] = await fixture.db.select().from(schema.universes);
  expect(row).toMatchObject({ name: "Research", lightspeedUniverseId: first, runtimeConfiguration: { members: [] } });
  expect(await fixture.db.select().from(schema.member)).toEqual([]);
  expect(await syncDeclaredUniverses(context)).toEqual({ adopted: 0, updated: 0 });
  expect(await fixture.db.select().from(schema.universes)).toEqual([row]);
});
it("updates names and declared access while preserving manual memberships and archived state", async () => {
  await syncDeclaredUniverses(context);
  const [row] = await fixture.db.select().from(schema.universes);
  const [user] = await fixture.db.select().from(schema.user);
  await fixture.db.insert(schema.member).values({ id: "manual", organizationId: row!.organizationId, userId: user!.id, role: "operator", createdAt: new Date() });
  await fixture.db.update(schema.universes).set({ status: "archived" }).where(eq(schema.universes.id, row!.id));
  inventory[0] = { ...inventory[0], displayName: "Renamed", directoryGroupMappings: [] };
  expect(await syncDeclaredUniverses(context)).toEqual({ adopted: 0, updated: 1 });
  expect((await fixture.db.select().from(schema.member))[0]?.role).toBe("operator");
  expect((await fixture.db.select().from(schema.universes))[0]).toMatchObject({ status: "archived", name: "Renamed", runtimeConfiguration: { directoryGroupMappings: [] } });
});
it("supports handle swaps atomically and retains the last projection on Runtime failure", async () => {
  inventory[1]!.displayName = "Second";
  await syncDeclaredUniverses(context);
  inventory[0]!.handle = "unnamed";
  inventory[1]!.handle = "research";
  await syncDeclaredUniverses(context);
  const organizations = await fixture.db.select().from(schema.organization);
  expect(organizations.find(o => o.name === "Research")?.slug).toBe("unnamed");
  vi.mocked(fetch).mockRejectedValue(new Error("offline"));
  await expect(syncDeclaredUniverses(context)).rejects.toThrow();
  expect(await fixture.db.select().from(schema.organization)).toEqual(organizations);
});
it("rolls back adoption when an unrelated organization holds its handle", async () => {
  await fixture.db.insert(schema.organization).values({ id: "unrelated", name: "Other", slug: "research", createdAt: new Date() });
  await expect(syncDeclaredUniverses(context)).rejects.toThrow();
  expect(await fixture.db.select().from(schema.universes)).toEqual([]);
  expect(await fixture.db.select().from(schema.organization)).toHaveLength(1);
});

it("marks deleting universes and allows another universe to take their handle", async () => {
  await syncDeclaredUniverses(context);
  const deletion = { startedAtMs: 1, requestedBy: {}, step: "workflows", detail: "Waiting for visibility" };
  inventory[0]!.deletion = deletion;
  inventory[1] = { ...inventory[1], displayName: "Replacement", handle: "research" };
  await syncDeclaredUniverses(context);
  const rows = await fixture.db.select().from(schema.universes);
  expect(rows.find(row => row.lightspeedUniverseId === first)).toMatchObject({ status: "deleting", runtimeConfiguration: { deletion, deletionHandle: "research" } });
  expect((await fixture.db.select().from(schema.organization)).find(row => row.name === "Replacement")?.slug).toBe("research");
});
it("keeps missing memberships on notFound but cascades them only on gone", async () => {
  await syncDeclaredUniverses(context);
  const [row] = await fixture.db.select().from(schema.universes);
  const [user] = await fixture.db.select().from(schema.user);
  await fixture.db.insert(schema.member).values({ id: "member", organizationId: row!.organizationId, userId: user!.id, role: "admin", createdAt: new Date() });
  await fixture.db.insert(schema.universeSetupInstallations).values({ universeId: row!.id, setupId: "setup" });
  inventory = [];
  await syncDeclaredUniverses(context);
  expect(await fixture.db.select().from(schema.member)).toHaveLength(1);
  expect(await fixture.db.select().from(schema.universeSetupInstallations)).toHaveLength(1);
  absentKind = "gone";
  await syncDeclaredUniverses(context);
  expect(await fixture.db.select().from(schema.organization)).toHaveLength(0);
  expect(await fixture.db.select().from(schema.universes)).toHaveLength(0);
  expect(await fixture.db.select().from(schema.member)).toHaveLength(0);
  expect(await fixture.db.select().from(schema.universeSetupInstallations)).toHaveLength(0);
});

it("accepts deletion without archiving, fences member routes and audits the request", async () => {
  await syncDeclaredUniverses(context);
  const [row] = await fixture.db.select().from(schema.universes);
  const [user] = await fixture.db.select().from(schema.user);
  await fixture.db.insert(schema.member).values({ id: "delete-member", organizationId: row!.organizationId, userId: user!.id, role: "admin", createdAt: new Date() });
  let role = "admin";
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => { c.set("session", { user: { id: user!.id, role } } as ApiVariables["session"]); await next(); });
  app.route("/", universeRoutes(context));
  const request = (handle: string) => app.request(`/${row!.id}`, { method: "DELETE", headers: { "content-type": "application/json" }, body: JSON.stringify({ handle }) });
  const runtime = vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
    expect(rpc.method).toBe("deployment/universes/delete");
    expect(new Headers(init.headers).get("x-lightspeed-actor")).toBe(user!.id);
    return Response.json({ id: rpc.id, result: { result: { universeId: first, state: "deleting" }, notifications: [] } });
  });
  vi.stubGlobal("fetch", runtime);
  expect((await request("wrong")).status).toBe(400);
  expect(runtime).not.toHaveBeenCalled();
  role = "user";
  expect((await request("research")).status).toBe(403);
  role = "admin";
  expect((await request("research")).status).toBe(200);
  expect((await fixture.db.select().from(schema.universes))[0]?.status).toBe("deleting");
  expect((await fixture.db.select().from(schema.organization))[0]?.slug).toBe(`_deleting-${first}`);
  expect((await fixture.db.select().from(schema.identityAudit)).at(-1)).toMatchObject({ action: "universe.delete", actorId: user!.id, targetId: first });
  expect((await app.request(`/${row!.id}`)).status).toBe(404);
  expect((await app.request(`/${row!.id}/engine`, { method: "POST" })).status).toBe(404);
  expect((await (await app.request("/")).json() as { status: string; handle: string }[])[0]).toMatchObject({ status: "deleting", handle: "research" });
  role = "user";
  expect(await (await app.request("/")).json()).toEqual([]);
});
it("does not remove Platform data when the delete request fails", async () => {
  await syncDeclaredUniverses(context);
  const [row] = await fixture.db.select().from(schema.universes);
  const [user] = await fixture.db.select().from(schema.user);
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => { c.set("session", { user: { id: user!.id, role: "admin" } } as ApiVariables["session"]); await next(); });
  app.route("/", universeRoutes(context));
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
    return Response.json({ id: rpc.id, error: { code: -32004, message: "not found" } });
  }));
  expect((await app.request(`/${row!.id}`, { method: "DELETE", headers: { "content-type": "application/json" }, body: JSON.stringify({ handle: "research" }) })).status).toBe(404);
  expect((await fixture.db.select().from(schema.universes))[0]?.status).toBe("active");
});
