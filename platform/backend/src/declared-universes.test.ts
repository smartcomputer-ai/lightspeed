import { decodeHttpRequest, problemResponse } from "../../test/http-fixture.js";
import { afterAll, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { eq } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { declaredPrincipalRole } from "./organization-access.js";
import { syncDeclaredUniverses } from "./declared-universes.js";
import { identityEnv, identityFixture } from "../test/identity-fixture.js";
import type { AppContext } from "./context.js";

const issuer = "https://identity.example.test";
it("combines scoped manual and exact company-group roles without granting deployment authority", () => {
  const identity = { email: "owner@example.test", emailVerified: true, oidcIssuer: issuer, oidcSubject: "owner", identitySource: "company", companyAdmitted: true, directoryGroups: ["research-readers"], banned: false } as typeof schema.user.$inferSelect;
  const roles = ["viewer", "contributor", "operator", "owner"] as const;
  const policy = { members: [{ email: "OWNER@example.test", role: "contributor" as const }, { issuer, subject: "owner", role: "operator" as const }], directoryGroupMappings: [{ issuer, group: "research-readers", role: "viewer" as const }] };
  expect(declaredPrincipalRole(policy, identity, roles)).toBe("operator");
  expect(declaredPrincipalRole(policy, identity, roles, "owner")).toBe("owner");
  expect(declaredPrincipalRole(policy, { ...identity, banned: true }, roles, "owner")).toBeUndefined();
  expect(declaredPrincipalRole(policy, { ...identity, emailVerified: false, oidcIssuer: "https://other.example" }, roles)).toBeUndefined();
  expect(declaredPrincipalRole({ directoryGroupMappings: policy.directoryGroupMappings }, { ...identity, directoryGroups: ["research-readers-extra"] }, roles)).toBeUndefined();
});
let fixture: Awaited<ReturnType<typeof identityFixture>>;
let context: AppContext;
let inventory: Record<string, unknown>[];
let absentKind: "gone" | "notFound";
const first = "11111111-1111-4111-8111-111111111111";
const second = "22222222-2222-4222-8222-222222222222";
beforeAll(async () => { fixture = await identityFixture({ ...identityEnv, lightspeedApiUrl: "https://runtime.example/api/v1", lightspeedApiToken: "runtime-test-token" }); context = { ...fixture, pool: {} as never }; }, 30000);
afterAll(async () => { await fixture?.postgres.close(); vi.unstubAllGlobals(); });
beforeEach(async () => {
  await fixture.postgres.exec('DELETE FROM "organization"'); absentKind = "notFound";
  inventory = [{ universeId: first, handle: "research", displayName: "Research", metadata: {}, protected: true, expiresAtMs: null }, { universeId: second, handle: "other", displayName: "Other", metadata: {}, protected: false, expiresAtMs: null }];
  vi.stubGlobal("fetch", vi.fn(async (url: string | Request | URL, init: RequestInit) => {
    const request = decodeHttpRequest(url, init);
    if (request.method === "deployment/universes/read") return problemResponse({ type: "about:blank", title: absentKind, detail: absentKind, status: absentKind === "gone" ? 410 : 404, kind: absentKind });
    return Response.json({ universes: inventory });
  }));
});
async function link(runtimeId = first, handle = "research") {
  const organizationId = crypto.randomUUID();
  await fixture.db.insert(schema.organization).values({ id: organizationId, name: "Organization", slug: organizationId, createdAt: new Date() });
  const [universe] = await fixture.db.insert(schema.universes).values({ organizationId: organizationId, lightspeedUniverseId: runtimeId, handle, name: "Old name", accessPolicy: { directoryGroupMappings: [{ issuer, group: "owners", role: "owner" }] } }).returning();
  return universe!;
}
it("never invents an organization or Owner grant for runtime documents", async () => {
  expect(await syncDeclaredUniverses(context)).toEqual({ adopted: 0, updated: 0 });
  expect(await fixture.db.select().from(schema.universes)).toEqual([]);
  expect(await fixture.db.select().from(schema.organization)).toEqual([]);
});
it("updates linked metadata without overwriting Platform access policy or organization identity", async () => {
  const universe = await link();
  expect(await syncDeclaredUniverses(context)).toEqual({ adopted: 0, updated: 1 });
  const [updated] = await fixture.db.select().from(schema.universes);
  expect(updated).toMatchObject({ name: "Research", accessPolicy: universe.accessPolicy, runtimeConfiguration: { protected: true } });
  expect((await fixture.db.select().from(schema.organization))[0]?.name).toBe("Organization");
});
it("removes only the universe after a permanent tombstone, preserving its organization and siblings", async () => {
  const firstUniverse = await link(); const secondUniverse = await link(second, "other");
  inventory = inventory.slice(1); absentKind = "gone";
  await syncDeclaredUniverses(context);
  expect(await fixture.db.select().from(schema.universes).where(eq(schema.universes.id, firstUniverse.id))).toEqual([]);
  expect((await fixture.db.select().from(schema.universes))[0]?.id).toBe(secondUniverse.id);
  expect(await fixture.db.select().from(schema.organization)).toHaveLength(2);
});
it("keeps linked resources on absence or upstream failure", async () => {
  const universe = await link(); inventory = [];
  await syncDeclaredUniverses(context);
  expect((await fixture.db.select().from(schema.universes))[0]?.id).toBe(universe.id);
  vi.mocked(fetch).mockRejectedValue(new Error("offline"));
  await expect(syncDeclaredUniverses(context)).rejects.toThrow("offline");
  expect((await fixture.db.select().from(schema.universes))[0]?.id).toBe(universe.id);
});
