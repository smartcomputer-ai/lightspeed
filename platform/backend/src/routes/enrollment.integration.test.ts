import { afterAll, beforeAll, expect, it } from "vitest";
import pg from "pg";
import { eq } from "drizzle-orm";
import { createDb, migrateDb, schema } from "@lightspeed-ai/platform-db";
import { identityEnv, identityFixture, identityTestApp, responseCookies } from "../../test/identity-fixture.js";
import { workloadIssuer } from "../../test/workload-issuer.js";
import { federationConfig } from "../federation-config.js";
import { WorkloadFederation } from "../federation.js";

let source: Awaited<ReturnType<typeof workloadIssuer>>;
let fixture: Awaited<ReturnType<typeof identityTestApp>>;
let closeDatabase: (() => Promise<void>) | undefined;
beforeAll(async () => {
  source = await workloadIssuer();
  const rules = [
    { subjectPattern: "system:serviceaccount:team-*:apply", maxOrganizations: 10, handlePattern: "^team-", organization: { universeQuota: 2 } },
    { subjectPattern: "limited-*", maxOrganizations: 1 },
    { subjectPattern: "exact", maxOrganizations: 1 },
  ].map(rule => ({ ...rule, issuer: "cluster" }));
  const federation = new WorkloadFederation(federationConfig.parse({ issuers: [source.trusted], enrollment: rules }));
  const env = { ...identityEnv, federation };
  const url = process.env.LIGHTSPEED_PLATFORM_MIGRATION_TEST_URL;
  if (url) {
    // The CI PostgreSQL service exercises simultaneous transactions on independent connections.
    const admin = new pg.Client({ connectionString: url });
    await admin.connect();
    const database = "enrollment_" + crypto.randomUUID().replaceAll("-", "");
    await admin.query(`CREATE DATABASE "${database}"`);
    const scratch = new URL(url);
    scratch.pathname = "/" + database;
    const handle = createDb(scratch.href);
    closeDatabase = async () => {
      await handle.pool.end();
      try { await admin.query(`DROP DATABASE "${database}"`); } finally { await admin.end(); }
    };
    await migrateDb(handle);
    fixture = await identityTestApp(handle.db, env);
  } else {
    const local = await identityFixture(env);
    fixture = local;
    closeDatabase = () => local.postgres.close();
  }
}, 30_000);
afterAll(async () => { await closeDatabase?.(); await source?.close(); });

const endpoint = "/api/v1/organizations/enroll";
function enroll(token: string, handle: string, extra: object = {}) {
  return fixture.app.request(endpoint, { method: "POST", headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: JSON.stringify({ handle, name: handle, ...extra }) });
}

it("admits only verified workloads and applies server-owned policy before creating anything", async () => {
  const cookie = responseCookies(await fixture.request("/api/auth/sign-in/email", { email: identityEnv.adminEmail, password: identityEnv.adminPassword }));
  expect((await fixture.request(endpoint, { name: "Human", handle: "human" }, cookie)).status).toBe(401);
  const response = await fixture.request("/api/v1/platform/credentials", { name: "Existing key" }, cookie);
  expect(response.status).toBe(201);
  const { secret } = await response.json() as { secret: string };
  expect((await enroll(secret, "from-key")).status).toBe(401);
  const subject = "system:serviceaccount:team-validation:apply";
  const token = await source.mint({ sub: subject });
  expect((await enroll(token, "team-valid", { universeQuota: 10000 })).status).toBe(400);
  expect((await enroll(token, "wrong-handle")).status).toBe(403);
  expect((await enroll(await source.mint({ sub: "exact-extra" }), "team-other")).status).toBe(403);
  expect((await enroll(await source.mint({ sub: "system:serviceaccount:team-a:other" }), "team-other")).status).toBe(403);
  await fixture.db.insert(schema.organization).values({ id: crypto.randomUUID(), slug: "team-existing", name: "Existing", createdAt: new Date() });
  expect((await enroll(token, "team-existing")).status).toBe(409);
  expect(await fixture.db.select().from(schema.organizationEnrollments).where(eq(schema.organizationEnrollments.subject, subject))).toHaveLength(0);
});

it("creates one ordinary organization, one admin binding and one enrollment audit, then resolves retries", async () => {
  const token = await source.mint();
  const responses = await Promise.all([enroll(token, "team-first"), enroll(token, "team-first")]);
  expect(responses.map(r => r.status).sort()).toEqual([200, 201]);
  const [first, retry] = await Promise.all(responses.sort((a, b) => b.status - a.status).map(r => r.json())) as [{ organizationId: string; enrolled: boolean }, unknown];
  expect(first.enrolled).toBe(true);
  expect(retry).toEqual({ organizationId: first.organizationId, enrolled: false });
  expect((await enroll(token, "team-second")).status).toBe(409);
  const [org] = await fixture.db.select().from(schema.organization).where(eq(schema.organization.id, first.organizationId));
  expect(org).toMatchObject({ slug: "team-first", personalUserId: null, universeQuota: 2, defaultProtected: true });
  const users = await fixture.db.select().from(schema.serviceUsers).where(eq(schema.serviceUsers.organizationId, first.organizationId));
  expect(users).toMatchObject([{ name: "deployer", role: "admin", enabled: true }]);
  const audit = await fixture.db.select().from(schema.identityAudit).where(eq(schema.identityAudit.action, "organization.enrolled"));
  expect(audit.filter(e => e.targetId === first.organizationId)).toMatchObject([{ actorId: `serviceUser:${users[0]!.id}`,
    details: { issuer: source.issuer, subject: source.subject, credentialKind: "federated" } }]);
  const result = await fixture.app.request("/api/v1/platform/organizations", { headers: { authorization: `Bearer ${token}` } });
  const workloadView = await result.json() as { organizations: object[] };
  expect(workloadView).toMatchObject({ canCreate: false, organizations: [{ id: first.organizationId, role: "admin" }] });
  expect(workloadView.organizations[0]).not.toHaveProperty("enrollment");
  const cookie = responseCookies(await fixture.request("/api/auth/sign-in/email", { email: identityEnv.adminEmail, password: identityEnv.adminPassword }));
  const inventory = await fixture.request("/api/v1/platform/organizations", undefined, cookie);
  const { organizations } = await inventory.json() as { organizations: { id: string; personal: boolean; enrollment?: object }[] };
  expect(organizations.find(o => o.id === first.organizationId)?.enrollment).toEqual({
    issuer: source.issuer, subject: source.subject, subjectPattern: "system:serviceaccount:team-*:apply",
  });
  expect(organizations.find(o => o.personal)).not.toHaveProperty("enrollment");
  const details = await fixture.request(`/api/v1/platform/organizations/${first.organizationId}`, undefined, cookie);
  expect(await details.json()).toMatchObject({ organization: { enrollment: {
    issuer: source.issuer, subject: source.subject, subjectPattern: "system:serviceaccount:team-*:apply",
  } } });
  await fixture.db.update(schema.serviceUsers).set({ enabled: false }).where(eq(schema.serviceUsers.id, users[0]!.id));
  expect((await enroll(token, "team-first")).status).toBe(401);
  await fixture.db.delete(schema.federatedCredentials).where(eq(schema.federatedCredentials.serviceUserId, users[0]!.id));
  expect((await enroll(token, "team-second")).status).toBe(409);
  await fixture.db.delete(schema.organization).where(eq(schema.organization.id, first.organizationId));
  expect((await enroll(token, "team-first")).status).toBe(201);
});

it("awards the last rule slot once and rolls back the losing enrollment", async () => {
  const requests = await Promise.all(["a", "b"].map(async id => enroll(await source.mint({ sub: "limited-" + id }), "limited-" + id)));
  expect(requests.map(r => r.status).sort()).toEqual([201, 409]);
  const enrolled = await fixture.db.select().from(schema.organizationEnrollments).where(eq(schema.organizationEnrollments.subjectPattern, "limited-*"));
  expect(enrolled).toHaveLength(1);
  const loser = requests[0]!.status === 409 ? "limited-a" : "limited-b";
  expect(await fixture.db.select().from(schema.organization).where(eq(schema.organization.slug, loser))).toHaveLength(0);
  expect(await fixture.db.select().from(schema.federatedCredentials).where(eq(schema.federatedCredentials.subject, loser))).toHaveLength(0);
});
