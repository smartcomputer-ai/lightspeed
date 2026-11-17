import { afterEach, beforeEach, expect, it } from "vitest";
import { eq } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { organizationFixture } from "../../test/organization-fixture.js";
import { userSubject } from "../../test/subject-fixture.js";
import { universeForPrincipal } from "./universes.js";

let fixture: Awaited<ReturnType<typeof organizationFixture>>;
beforeEach(async () => { fixture = await organizationFixture(); }, 30000);
afterEach(async () => { await fixture?.postgres.close(); });
const grants = () => `/universes/${fixture.universe.lightspeedUniverseId}/grants`;

it("allows an organization Member who is an Owner to manage scoped universe grants", async () => {
  await fixture.asUser("owner");
  const response = await fixture.request(grants(), "PUT", { principal: { kind: "user", id: "creator" }, role: "operator" });
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject({ grant: { principal: { kind: "user", id: "creator" }, role: "operator" } });
  const membership = await fixture.db.select().from(schema.member).where(eq(schema.member.userId, "creator"));
  expect(membership[0]?.role).toBe("creator");
});
it("keeps the last explicit Owner even for an organization admin", async () => {
  await fixture.asUser("organization-admin");
  expect((await fixture.request(`${grants()}/${fixture.ownerGrant.id}`, "DELETE")).status).toBe(409);
  expect((await fixture.request(grants(), "PUT", { principal: { kind: "user", id: "owner" }, role: "viewer" })).status).toBe(409);
});
it("supports several Owners and removes the creator's authority when its grant is removed", async () => {
  expect((await fixture.request(grants(), "PUT", { principal: { kind: "user", id: "creator" }, role: "owner" })).status).toBe(200);
  expect((await fixture.request(`${grants()}/${fixture.ownerGrant.id}`, "DELETE")).status).toBe(200);
  expect(await universeForPrincipal(fixture.context, userSubject("owner"), fixture.universe.id)).toBeNull();
  expect((await fixture.db.select().from(schema.universes))[0]?.createdBy).toBe("owner");
});
it("requires organization membership before granting a universe role", async () => {
  expect((await fixture.request(grants(), "PUT", { principal: { kind: "user", id: "other" }, role: "owner" })).status).toBe(400);
});
it("immediately disables universe access on organization membership removal without deleting resources", async () => {
  await fixture.asUser("organization-admin");
  expect((await fixture.request("/organizations/organization-a/members/owner", "DELETE")).status).toBe(200);
  expect(await universeForPrincipal(fixture.context, userSubject("owner"), fixture.universe.id)).toBeNull();
  expect(await fixture.db.select().from(schema.universes)).toHaveLength(1);
  expect(await fixture.db.select().from(schema.universeGrants).where(eq(schema.universeGrants.id, fixture.ownerGrant.id))).toHaveLength(1);
});
it("does not grant a Creator access to unrelated universes", async () => {
  expect(await universeForPrincipal(fixture.context, userSubject("creator"), fixture.universe.id)).toBeNull();
});
it("limits Owner directory access to principals in the containing organization", async () => {
  const response = await fixture.request("/organizations/organization-a/members");
  expect(response.status).toBe(200);
  const body = await response.json() as { members: Array<{ userId: string }> };
  expect(body.members.map(member => member.userId)).not.toContain("other");
  expect((await fixture.request("/organizations/organization-b/members")).status).toBe(404);
  await fixture.asUser("viewer");
  expect((await fixture.request("/organizations/organization-a/members")).status).toBe(403);
});
it("keeps a universe credential's limits even when its user is a Platform Admin", async () => {
  const subject = await fixture.asUser("owner", true);
  fixture.setSubject({ ...subject, credential: { kind: "personalKey", id: "limited", restrictions: { universeIds: [fixture.universe.lightspeedUniverseId] } } });
  expect((await fixture.request("/organizations/organization-a", "PATCH", { universeQuota: 1000 })).status).toBe(403);
  expect((await fixture.request(grants(), "PUT", { principal: { kind: "user", id: "creator" }, role: "viewer" })).status).toBe(200);
});
it("checks service user identity and suspension without manufacturing a human session", async () => {
  const [serviceUser] = await fixture.db.insert(schema.serviceUsers).values({ organizationId: "organization-a", name: "Worker", role: "member" }).returning();
  await fixture.db.insert(schema.universeGrants).values({ universeId: fixture.universe.id, serviceUserId: serviceUser!.id, role: "owner" });
  const auth = { principal: { kind: "serviceUser" as const, id: serviceUser!.id, serviceUser: serviceUser!, organizationId: "organization-a", platformAdmin: false as const }, actorId: `serviceUser:${serviceUser!.id}`, credential: { kind: "serviceUserKey" as const, id: "app-key", restrictions: {} } };
  fixture.setSubject(auth);
  expect((await fixture.request(grants())).status).toBe(200);
  await fixture.db.update(schema.serviceUsers).set({ enabled: false }).where(eq(schema.serviceUsers.id, serviceUser!.id));
  expect(await universeForPrincipal(fixture.context, auth, fixture.universe.id)).toBeNull();
});
