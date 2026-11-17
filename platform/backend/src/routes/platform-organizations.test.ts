import { afterEach, beforeEach, expect, it } from "vitest";
import { eq } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { organizationFixture } from "../../test/organization-fixture.js";

let fixture: Awaited<ReturnType<typeof organizationFixture>>;
beforeEach(async () => { fixture = await organizationFixture(); }, 30000);
afterEach(async () => { await fixture?.postgres.close(); });

type Listed = { organizations: Array<{ id: string; name: string; personal: boolean; universeCount?: number; universeQuota: number }> };
const list = async () => (await (await fixture.request("/organizations")).json()) as Listed;

it("counts each organization's universes against its quota in one inventory read", async () => {
  await personalOrganization();
  await fixture.db.insert(schema.universes).values({ organizationId: "organization-a", name: "Pending", handle: "pending", lightspeedUniverseId: "22222222-2222-4222-8222-222222222222", createdBy: "owner", status: "provisioning", runtimeConfiguration: { protected: false } });
  await fixture.asUser("platform-admin", true);
  const organizations = new Map((await list()).organizations.map(organization => [organization.id, organization]));
  expect(organizations.get("organization-a")).toMatchObject({ personal: false, universeCount: 2, universeQuota: 2 });
  expect(organizations.get("organization-b")).toMatchObject({ personal: false, universeCount: 0 });
  expect(organizations.get("personal-other")).toMatchObject({ personal: true, universeCount: 0 });
});

it("keeps universe usage out of a plain member's view", async () => {
  await fixture.asUser("viewer");
  const [organization] = (await list()).organizations;
  expect(organization).toMatchObject({ id: "organization-a", role: "member" });
  expect(organization).not.toHaveProperty("universeCount");
  await fixture.asUser("organization-admin");
  expect(await (await fixture.request("/organizations/organization-a")).json()).toMatchObject({ organization: { universeCount: 1 } });
});

it("protects new universes by default in a created organization", async () => {
  await fixture.asUser("organization-admin", true);
  const response = await fixture.request("/organizations", "POST", { name: "Research", slug: "research" });
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject({ organization: { personal: false, defaultProtected: true, universeCount: 0 } });
});

async function personalOrganization() {
  await fixture.db.insert(schema.organization).values({ id: "personal-other", name: "other", slug: "personal-other", personalUserId: "other", createdAt: new Date() });
  await fixture.db.insert(schema.member).values({ id: "personal-other-owner", organizationId: "personal-other", userId: "other", role: "admin", createdAt: new Date() });
}

it("keeps a personal organization to its owner while allowing their service users", async () => {
  await personalOrganization();
  await fixture.asUser("other");
  expect((await fixture.request("/organizations/personal-other/members/creator", "PUT", { role: "member" })).status).toBe(409);
  expect((await fixture.request("/organizations/personal-other/members/other", "DELETE")).status).toBe(409);
  expect((await fixture.request("/organizations/personal-other", "PATCH", { name: "Renamed" })).status).toBe(409);
  expect((await fixture.request("/organizations/personal-other", "PATCH", { membershipPolicy: { members: [{ email: "creator@example.test", role: "member" }] } })).status).toBe(409);
  expect((await fixture.request("/organizations/personal-other", "PATCH", { defaultProtected: true })).status).toBe(200);
  expect((await fixture.request("/organizations/personal-other/service-users", "POST", { name: "CI" })).status).toBe(200);
  expect(await fixture.db.select().from(schema.member).where(eq(schema.member.organizationId, "personal-other"))).toHaveLength(1);
});

it("lets only a Platform Admin change an organization's universe quota", async () => {
  await fixture.asUser("organization-admin");
  expect((await fixture.request("/organizations/organization-a", "PATCH", { universeQuota: 100000 })).status).toBe(403);
  await fixture.asUser("platform-admin", true);
  const response = await fixture.request("/organizations/organization-a", "PATCH", { universeQuota: 5 });
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject({ organization: { universeQuota: 5 } });
});

it("presents a personal organization under its owner's current name", async () => {
  await personalOrganization();
  await fixture.db.update(schema.user).set({ name: "Ada Lovelace" }).where(eq(schema.user.id, "other"));
  await fixture.asUser("other");
  expect((await list()).organizations.find(organization => organization.id === "personal-other")).toMatchObject({ name: "Ada Lovelace", personal: true });
  expect(await (await fixture.request("/organizations/personal-other")).json()).toMatchObject({ organization: { name: "Ada Lovelace" } });
});
