import { afterAll, beforeAll, expect, it } from "vitest";
import { eq } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { identityEnv, identityFixture } from "../test/identity-fixture.js";
import { workloadIssuer } from "../test/workload-issuer.js";

let source: Awaited<ReturnType<typeof workloadIssuer>>;
let fixture: Awaited<ReturnType<typeof identityFixture>>;
beforeAll(async () => {
  source = await workloadIssuer();
  fixture = await identityFixture({ ...identityEnv, federation: source.federation });
}, 30_000);
afterAll(async () => { await fixture?.postgres.close(); await source?.close(); });

it("admits a bound service user with current restrictions and enforces disablement, UID pinning and deletion on the next request", async () => {
  const token = await source.mint({ "kubernetes.io": { serviceaccount: { uid: "uid-1" } } });
  const request = (path: string, bearer = token, method = "GET", body?: object) =>
    fixture.app.request(path, { method, headers: { authorization: `Bearer ${bearer}`, "content-type": "application/json" }, body: JSON.stringify(body) });
  const session = "/api/v1/platform/session";
  expect((await request(session)).status).toBe(401);
  const organizationId = crypto.randomUUID();
  await fixture.db.insert(schema.organization).values({ id: organizationId, slug: "federated", name: "Federated", createdAt: new Date() });
  const [user] = await fixture.db.insert(schema.serviceUsers).values({ organizationId, name: "CI", role: "admin" }).returning();
  const [binding] = await fixture.db.insert(schema.federatedCredentials).values({
    serviceUserId: user!.id, issuer: source.issuer, subject: source.subject, serviceAccountUid: "uid-1", restrictions: { readOnly: true },
  }).returning();
  expect(await (await request(session)).json()).toMatchObject({ principal: { kind: "serviceUser", id: user!.id }, credential: { kind: "federated", id: binding!.id } });
  expect((await request("/api/ui/me")).status).toBe(401);
  expect((await request(`/api/v1/platform/organizations/${organizationId}`, token, "PATCH", { name: "New" })).status).toBe(403);
  expect((await request(session, await source.mint())).status).toBe(401);
  const audit = await fixture.db.select().from(schema.identityAudit).where(eq(schema.identityAudit.action, "serviceUser.authenticate"));
  expect(audit[0]?.details).toMatchObject({ credentialKind: "federated", issuer: source.issuer, subject: source.subject });
  expect(JSON.stringify(audit)).not.toContain(token);
  await fixture.db.update(schema.serviceUsers).set({ enabled: false }).where(eq(schema.serviceUsers.id, user!.id));
  expect((await request(session)).status).toBe(401);
  await fixture.db.update(schema.serviceUsers).set({ enabled: true }).where(eq(schema.serviceUsers.id, user!.id));
  await fixture.db.delete(schema.federatedCredentials).where(eq(schema.federatedCredentials.id, binding!.id));
  expect((await request(session)).status).toBe(401);
});
