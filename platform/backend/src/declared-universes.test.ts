import { afterAll, beforeAll, beforeEach, expect, it, vi } from "vitest";
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
const first = "11111111-1111-4111-8111-111111111111";
const second = "22222222-2222-4222-8222-222222222222";
beforeAll(async () => {
  fixture = await identityFixture({ ...identityEnv, lightspeedApiUrl: "https://runtime.example/rpc", lightspeedApiKey: "lsk_fixture" });
  context = { ...fixture, pool: {} as never };
}, 30000);
afterAll(async () => { await fixture?.postgres.close(); vi.unstubAllGlobals(); });
beforeEach(async () => {
  await fixture.postgres.exec('DELETE FROM "organization"');
  inventory = [{ universeId: first, handle: "research", displayName: "Research", members: [], directoryGroupMappings: [{ issuer, group: "research-readers", role: "viewer" }] }, { universeId: second, handle: "unnamed" }];
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
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
