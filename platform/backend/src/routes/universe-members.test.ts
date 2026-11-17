import { afterEach, beforeEach, expect, it } from "vitest";
import { eq } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { organizationFixture } from "../../test/organization-fixture.js";
import { serviceUserSubject, userSubject } from "../../test/subject-fixture.js";
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
  // The creator keeps only what every member of the organization holds.
  const remaining = await universeForPrincipal(fixture.context, userSubject("owner"), fixture.universe.id);
  expect(remaining?.role).toBe("viewer");
  expect(remaining?.actions.has("administer")).toBe(false);
  expect((await fixture.db.select().from(schema.universes))[0]?.createdBy).toBe("owner");
});
it("requires organization membership before granting a universe role", async () => {
  expect((await fixture.request(grants(), "PUT", { principal: { kind: "user", id: "other" }, role: "owner" })).status).toBe(400);
});
it("removes universe access and this organization's universe roles with the membership, keeping the universes", async () => {
  await fixture.db.insert(schema.member).values({ id: "owner-in-b", organizationId: "organization-b", userId: "owner", role: "member", createdAt: new Date() });
  const [elsewhere] = await fixture.db.insert(schema.universes).values({ organizationId: "organization-b", name: "Elsewhere", handle: "elsewhere", lightspeedUniverseId: "22222222-2222-4222-8222-222222222222", createdBy: "other", runtimeConfiguration: { protected: false } }).returning();
  await fixture.db.insert(schema.universeGrants).values({ universeId: elsewhere!.id, userId: "owner", role: "operator" });
  await fixture.asUser("organization-admin");
  expect((await fixture.request("/organizations/organization-a/members/owner", "DELETE")).status).toBe(200);
  expect(await universeForPrincipal(fixture.context, userSubject("owner"), fixture.universe.id)).toBeNull();
  expect(await fixture.db.select().from(schema.universes)).toHaveLength(2);
  const remaining = await fixture.db.select().from(schema.universeGrants).where(eq(schema.universeGrants.userId, "owner"));
  expect(remaining.map(grant => grant.universeId)).toEqual([elsewhere!.id]);
});
it("keeps universe roles when a directory declaration keeps the person a member", async () => {
  await fixture.db.update(schema.organization).set({ membershipPolicy: { members: [{ email: "owner@example.test", role: "member" }] } }).where(eq(schema.organization.id, "organization-a"));
  await fixture.asUser("organization-admin");
  expect((await fixture.request("/organizations/organization-a/members/owner", "DELETE")).status).toBe(200);
  expect(await fixture.db.select().from(schema.universeGrants).where(eq(schema.universeGrants.id, fixture.ownerGrant.id))).toHaveLength(1);
  expect((await universeForPrincipal(fixture.context, userSubject("owner"), fixture.universe.id))?.role).toBe("owner");
});
it("marks a role whose holder left without a removal, and never counts it as an Owner", async () => {
  expect((await fixture.request(grants(), "PUT", { principal: { kind: "user", id: "creator" }, role: "owner" })).status).toBe(200);
  // A directory change drops the membership with no removal to clean up after.
  await fixture.db.delete(schema.member).where(eq(schema.member.userId, "creator"));
  const listed = await (await fixture.request(grants())).json() as { grants: { principal: { id: string }; active: boolean }[] };
  expect(Object.fromEntries(listed.grants.map(grant => [grant.principal.id, grant.active]))).toEqual({ owner: true, viewer: true, creator: false });
  expect((await fixture.request(`${grants()}/${fixture.ownerGrant.id}`, "DELETE")).status).toBe(409);
  expect((await fixture.request(grants(), "PUT", { principal: { kind: "user", id: "owner" }, role: "viewer" })).status).toBe(409);
});
it("gives a Creator only the organization's base role on universes it did not create", async () => {
  const access = await universeForPrincipal(fixture.context, userSubject("creator"), fixture.universe.id);
  expect(access?.role).toBe("viewer");
  expect([...access!.actions]).toEqual(["read"]);
  await fixture.db.update(schema.organization).set({ baseUniverseRole: null }).where(eq(schema.organization.id, "organization-a"));
  expect(await universeForPrincipal(fixture.context, userSubject("creator"), fixture.universe.id)).toBeNull();
});
it("adds the organization's base role to a member's explicit grant", async () => {
  await fixture.db.update(schema.organization).set({ baseUniverseRole: "contributor" }).where(eq(schema.organization.id, "organization-a"));
  expect((await universeForPrincipal(fixture.context, userSubject("creator"), fixture.universe.id))?.role).toBe("contributor");
  // An explicit grant above the base role wins; one below it adds nothing.
  expect((await universeForPrincipal(fixture.context, userSubject("viewer"), fixture.universe.id))?.role).toBe("contributor");
  expect((await universeForPrincipal(fixture.context, userSubject("owner"), fixture.universe.id))?.role).toBe("owner");
});
it("can make every member an Owner of every universe", async () => {
  await fixture.db.update(schema.organization).set({ baseUniverseRole: "owner" }).where(eq(schema.organization.id, "organization-a"));
  const access = await universeForPrincipal(fixture.context, userSubject("creator"), fixture.universe.id);
  expect(access?.role).toBe("owner");
  expect(access?.actions.has("administer")).toBe(true);
  expect(access?.actions.has("accessPrivateSessions")).toBe(true);
});
it("keeps service users to their explicit grants", async () => {
  const [robot] = await fixture.db.insert(schema.serviceUsers).values({ organizationId: "organization-a", name: "CI", role: "member" }).returning();
  expect(await universeForPrincipal(fixture.context, serviceUserSubject(robot!), fixture.universe.id)).toBeNull();
});
it("does not extend the base role to a Platform Admin outside the organization", async () => {
  const access = await universeForPrincipal(fixture.context, userSubject("other", true), fixture.universe.id);
  expect(access?.role).toBe("owner");
  expect(access?.organizationRole).toBe("admin");
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

it("reads a universe for anyone with access, and its declared access policy for its Owners only", async () => {
  await fixture.asUser("owner");
  expect((await fixture.request(`/universes/${fixture.universe.lightspeedUniverseId}/access-policy`, "PUT", { policy: { members: [{ email: "dana@example.test", role: "operator" }] } })).status).toBe(200);
  const read = await fixture.request(`/universes/${fixture.universe.lightspeedUniverseId}`);
  expect(read.status).toBe(200);
  expect(await read.json()).toMatchObject({ universe: { organizationId: "organization-a", handle: "work" } });
  expect(await (await fixture.request(`/universes/${fixture.universe.lightspeedUniverseId}/access-policy`)).json()).toEqual({ policy: { members: [{ email: "dana@example.test", role: "operator" }] } });
  await fixture.asUser("viewer");
  expect((await fixture.request(`/universes/${fixture.universe.lightspeedUniverseId}`)).status).toBe(200);
  expect((await fixture.request(`/universes/${fixture.universe.lightspeedUniverseId}/access-policy`)).status).toBe(403);
});

it("keeps service user names unique in an organization", async () => {
  await fixture.asUser("organization-admin");
  expect((await fixture.request("/organizations/organization-a/service-users", "POST", { name: "Deployer", role: "member" })).status).toBe(200);
  expect((await fixture.request("/organizations/organization-a/service-users", "POST", { name: "Deployer", role: "member" })).status).toBe(409);
});
