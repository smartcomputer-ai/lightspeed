import { afterAll, beforeAll, expect, it, vi } from "vitest";
import { eq } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { identityFixture, identityEnv, responseCookies } from "../test/identity-fixture.js";
import { authenticateRequest } from "./authentication.js";
import { decodeHttpRequest } from "../../test/http-fixture.js";
import { enforceRestrictions } from "./auth-context.js";
import type { AppContext } from "./context.js";

let fixture: Awaited<ReturnType<typeof identityFixture>>;
let cookie: string;
let userId: string;
const context = (): AppContext => ({ ...fixture, pool: {} as never });
const admission = (secret: string) => authenticateRequest(context(), new Request(`${fixture.env.baseUrl}/api/v1/platform/organizations`, { headers: { authorization: `Bearer ${secret}` } }));

beforeAll(async () => {
  fixture = await identityFixture({ ...identityEnv, oidc: null });
  const response = await fixture.request("/api/auth/sign-in/email", { email: identityEnv.adminEmail, password: identityEnv.adminPassword });
  cookie = responseCookies(response);
  userId = (await fixture.db.select().from(schema.user).where(eq(schema.user.email, identityEnv.adminEmail!)))[0]!.id;
}, 30000);
afterAll(async () => { await fixture?.postgres.close(); });

it("creates an expiring hashed personal key and never turns it into a browser session", async () => {
  const response = await fixture.request("/api/v1/platform/credentials", { name: "Automation", restrictions: { readOnly: true } }, cookie);
  expect(response.status).toBe(201);
  const created = await response.json() as { credential: { id: string; expiresAt: string }; secret: string };
  expect(created.secret).toMatch(/^lsp_/);
  for (const field of ["key", "referenceId", "permissions", "metadata", "configId"]) expect(created.credential).not.toHaveProperty(field);
  const days = (Date.parse(created.credential.expiresAt) - Date.now()) / 86400000;
  expect(days).toBeGreaterThan(364); expect(days).toBeLessThanOrEqual(365);
  const [stored] = await fixture.db.select().from(schema.apikey).where(eq(schema.apikey.id, created.credential.id));
  expect(stored?.key).not.toBe(created.secret);
  expect(stored?.referenceId).toBe(userId);
  const result = await admission(created.secret);
  expect(result?.auth).toMatchObject({ principal: { kind: "user", id: userId }, credential: { kind: "personalKey", restrictions: { readOnly: true } } });
  expect(result?.session).toBeUndefined();
  const listed = await fixture.request("/api/v1/platform/credentials", undefined, cookie);
  expect(await listed.text()).not.toContain(created.secret);
  const refused = await fixture.app.request(new Request(`${fixture.env.baseUrl}/api/v1/platform/credentials`, { method: "POST", headers: { authorization: `Bearer ${created.secret}`, "content-type": "application/json" }, body: JSON.stringify({ name: "Broader" }) }));
  expect(refused.status).toBe(403);
  expect((await fixture.request(`/api/v1/platform/credentials/${created.credential.id}`, undefined, cookie, "DELETE")).status).toBe(200);
  expect(await admission(created.secret)).toBeNull();
});

it("binds organization keys to service users without giving their creator membership", async () => {
  const organizationId = crypto.randomUUID();
  await fixture.db.insert(schema.organization).values({ id: organizationId, name: "Automation", slug: organizationId, createdAt: new Date() });
  const [serviceUser] = await fixture.db.insert(schema.serviceUsers).values({ organizationId: organizationId, name: "Worker", role: "creator" }).returning();
  const response = await fixture.request(`/api/v1/platform/organizations/${organizationId}/service-users/${serviceUser!.id}/credentials`, { name: "Worker key" }, cookie);
  expect(response.status).toBe(201);
  const created = await response.json() as { credential: { id: string }; secret: string };
  expect(created.secret).toMatch(/^lsa_/);
  expect((await fixture.db.select().from(schema.member).where(eq(schema.member.organizationId, organizationId))).length).toBe(0);
  const result = await admission(created.secret);
  expect(result?.auth).toMatchObject({ principal: { kind: "serviceUser", id: serviceUser!.id, organizationId, platformAdmin: false }, actorId: `serviceUser:${serviceUser!.id}` });
  expect(result?.session).toBeUndefined();
  await fixture.db.update(schema.serviceUsers).set({ enabled: false }).where(eq(schema.serviceUsers.id, serviceUser!.id));
  expect(await admission(created.secret)).toBeNull();
  await fixture.db.update(schema.serviceUsers).set({ enabled: true }).where(eq(schema.serviceUsers.id, serviceUser!.id));
  await fixture.db.delete(schema.credentialBindings).where(eq(schema.credentialBindings.keyId, created.credential.id));
  expect(await admission(created.secret)).toBeNull();
});

it("does not accept an unknown bearer through a valid cookie or expose plugin key management", async () => {
  expect(await authenticateRequest(context(), new Request(`${fixture.env.baseUrl}/api/v1/platform/organizations`, { headers: { cookie, authorization: "Bearer invalid" } }))).toBeNull();
  expect((await fixture.request("/api/auth/api-key/create", { name: "Unreviewed" }, cookie)).status).toBe(404);
});

it("installs only a service user Operator credential scoped to configuration in one universe", async () => {
  const organizationId = crypto.randomUUID();
  const universeId = crypto.randomUUID();
  await fixture.db.insert(schema.organization).values({ id: organizationId, name: "Configurator", slug: organizationId, createdAt: new Date() });
  const [universe] = await fixture.db.insert(schema.universes).values({ organizationId: organizationId, lightspeedUniverseId: universeId, handle: `config-${organizationId}`, name: "Configurable" }).returning();
  fixture.env.configuratorMcpUrl = "https://configurator.example/mcp";
  fixture.env.lightspeedApiUrl = "https://runtime.example/api/v1";
  fixture.env.lightspeedApiToken = "lsr_infrastructure_only";
  let issuedSecret: string | undefined;
  let failInstallation = false;
  vi.stubGlobal("fetch", vi.fn(async (input: string | Request | URL, init?: RequestInit) => {
    const request = decodeHttpRequest(input, init);
    expect(request.universe).toBe(universeId);
    if (request.operation === "credentials/grants/import") {
      issuedSecret = request.params.token;
      expect(issuedSecret).toMatch(/^lsa_/);
      expect(issuedSecret).not.toBe(fixture.env.lightspeedApiToken);
      expect(request.params.exposure).toBe("brokered");
      return Response.json({ grant: { grantId: request.params.grantId } });
    }
    if (request.operation === "mcp/servers/put") {
      expect(request.params.server).toMatchObject({ authPolicy: { type: "requiredBearer" }, credential: { type: "authGrant" } });
      if (failInstallation) { failInstallation = false; throw new Error("installation failure"); }
      return Response.json({ server: request.params.server });
    }
    throw new Error(`Unexpected operation ${request.operation}`);
  }));
  try {
    const path = `/api/ui/universes/${universe!.id}/setups/configurator/install`;
    const installed = await fixture.request(path, {}, cookie);
    expect(installed.status).toBe(200);
    expect(await installed.text()).not.toContain(issuedSecret!);
    const first = issuedSecret!;
    const admissionResult = await admission(first);
    expect(admissionResult?.auth.principal.kind).toBe("serviceUser");
    const auth = admissionResult!.auth;
    expect(auth.credential.restrictions).toMatchObject({ organizationIds: [organizationId], universeIds: [universeId], allowUniverseCreation: false });
    const [serviceUser] = await fixture.db.select().from(schema.serviceUsers).where(eq(schema.serviceUsers.id, auth.principal.id));
    expect(serviceUser?.role).toBe("member");
    const [grant] = await fixture.db.select().from(schema.universeGrants).where(eq(schema.universeGrants.serviceUserId, auth.principal.id));
    expect(grant?.role).toBe("operator");
    expect(() => enforceRestrictions(auth, { operation: "profiles/put", organizationId, universeId, write: true })).not.toThrow();
    expect(() => enforceRestrictions(auth, { operation: "session/start", organizationId, universeId, write: true })).toThrow();
    expect(() => enforceRestrictions(auth, { operation: "profiles/read", organizationId, universeId: crypto.randomUUID() })).toThrow();
    failInstallation = true;
    expect((await fixture.request(path, {}, cookie)).status).toBeGreaterThanOrEqual(400);
    expect(await admission(first)).not.toBeNull();
    expect(await admission(issuedSecret!)).toBeNull();
    const [retained] = await fixture.db.select().from(schema.universeSetupInstallations).where(eq(schema.universeSetupInstallations.universeId, universe!.id));
    expect(retained?.status).toBe("ready");
    expect(retained?.state.serviceUserId).toBe(auth.principal.id);
    expect((await fixture.request(path, {}, cookie)).status).toBe(200);
    expect(await admission(first)).toBeNull();
    expect(await admission(issuedSecret!)).not.toBeNull();
  } finally {
    vi.unstubAllGlobals();
    fixture.env.configuratorMcpUrl = null;
    fixture.env.lightspeedApiUrl = null;
    fixture.env.lightspeedApiToken = null;
  }
});

it("bounds credential request bodies before JSON decoding", async () => {
  const response = await fixture.request("/api/v1/platform/credentials", { name: "x".repeat(300_000) }, cookie);
  expect(response.status).toBe(413);
  expect(response.headers.get("content-type")).toContain("application/problem+json");
});

it("returns retryable rate exhaustion without treating a verified credential as invalid", async () => {
  const response = await fixture.request("/api/v1/platform/credentials", { name: "Rate limited" }, cookie);
  const created = await response.json() as { credential: { id: string }; secret: string };
  await fixture.db.update(schema.apikey).set({ rateLimitMax: 1, rateLimitTimeWindow: 60_000, lastRequest: null, requestCount: 0 }).where(eq(schema.apikey.id, created.credential.id));
  expect(await admission(created.secret)).not.toBeNull();
  const limited = await fixture.app.request(new Request(`${fixture.env.baseUrl}/api/v1/platform/organizations`, { headers: { authorization: `Bearer ${created.secret}` } }));
  expect(limited.status).toBe(429);
  expect(limited.headers.get("content-type")).toContain("application/problem+json");
  expect(Number(limited.headers.get("retry-after"))).toBeGreaterThan(0);
  expect(Number(limited.headers.get("retry-after"))).toBeLessThanOrEqual(60);
  expect(await limited.json()).toMatchObject({ status: 429, kind: "rejected" });
});

it("creates a key without expiry only when asked explicitly and the deployment sets no maximum", async () => {
  const defaulted = await (await fixture.request("/api/v1/platform/credentials", { name: "Default expiry", restrictions: {} }, cookie)).json() as { credential: { expiresAt: string | null } };
  expect(defaulted.credential.expiresAt).not.toBeNull();
  const response = await fixture.request("/api/v1/platform/credentials", { name: "Forever", expiresInDays: null, restrictions: {} }, cookie);
  expect(response.status).toBe(201);
  const created = await response.json() as { credential: { id: string; expiresAt: string | null }; secret: string };
  expect(created.credential.expiresAt).toBeNull();
  expect((await admission(created.secret))?.auth.credential.id).toBe(created.credential.id);
  const listed = await (await fixture.request("/api/v1/platform/credentials", undefined, cookie)).json() as { credentials: Array<{ id: string; expiresAt: string | null }> };
  expect(listed.credentials.find(credential => credential.id === created.credential.id)?.expiresAt).toBeNull();
  fixture.env.apiKeyMaxExpiryDays = 30;
  try {
    const refused = await fixture.request("/api/v1/platform/credentials", { name: "Forever again", expiresInDays: null, restrictions: {} }, cookie);
    expect(refused.status).toBe(400);
    expect((await fixture.request("/api/v1/platform/credentials", { name: "Too long", expiresInDays: 31, restrictions: {} }, cookie)).status).toBe(400);
  } finally {
    delete fixture.env.apiKeyMaxExpiryDays;
  }
});

it("lists every key that can reach an organization, with whose key it is", async () => {
  const organizationId = crypto.randomUUID();
  await fixture.db.insert(schema.organization).values({ id: organizationId, name: "Audit", slug: organizationId, createdAt: new Date() });
  const [serviceUser] = await fixture.db.insert(schema.serviceUsers).values({ organizationId, name: "Deployer", role: "member" }).returning();
  expect((await fixture.request(`/api/v1/platform/organizations/${organizationId}/service-users/${serviceUser!.id}/credentials`, { name: "Deployer key" }, cookie)).status).toBe(201);
  const create = (name: string, restrictions: Record<string, unknown>) => fixture.request("/api/v1/platform/credentials", { name, restrictions }, cookie);
  expect((await create("Here", { organizationIds: [organizationId] })).status).toBe(201);
  expect((await create("Elsewhere", { organizationIds: ["some-other-organization"] })).status).toBe(201);
  const response = await fixture.request(`/api/v1/platform/organizations/${organizationId}/credentials`, undefined, cookie);
  expect(response.status).toBe(200);
  const { credentials } = await response.json() as { credentials: Array<{ name: string; owner: { kind: string; name: string } }> };
  const names = credentials.map(credential => credential.name);
  expect(names).toContain("Deployer key");
  expect(names).toContain("Here");
  expect(names).not.toContain("Elsewhere");
  expect(credentials.find(credential => credential.name === "Deployer key")?.owner).toMatchObject({ kind: "serviceUser", name: "Deployer" });
  expect(credentials.find(credential => credential.name === "Here")?.owner).toMatchObject({ kind: "user" });
});

it("gives a key a new secret in place: same key, old secret refused at once", async () => {
  const created = await (await fixture.request("/api/v1/platform/credentials", { name: "Replaceable", restrictions: { readOnly: true } }, cookie)).json() as { credential: { id: string }; secret: string };
  const response = await fixture.request(`/api/v1/platform/credentials/${created.credential.id}/replace`, {}, cookie);
  expect(response.status).toBe(200);
  const replaced = await response.json() as { credential: { id: string; name: string; restrictions: unknown; start: string }; secret: string };
  expect(replaced.credential).toMatchObject({ id: created.credential.id, name: "Replaceable", restrictions: { readOnly: true } });
  expect(replaced.secret).toMatch(/^lsp_[A-Za-z]{64}$/);
  expect(replaced.credential.start).toBe(replaced.secret.slice(0, 12));
  expect(await admission(created.secret)).toBeNull();
  expect((await admission(replaced.secret))?.auth).toMatchObject({ principal: { kind: "user", id: userId }, credential: { id: created.credential.id } });
  await fixture.request(`/api/v1/platform/credentials/${created.credential.id}`, undefined, cookie, "DELETE");
  expect((await fixture.request(`/api/v1/platform/credentials/${created.credential.id}/replace`, {}, cookie)).status).toBe(404);
});

it("declares a service user's key with a supplied secret, and reports and makes only what changed", async () => {
  const organizationId = crypto.randomUUID();
  await fixture.db.insert(schema.organization).values({ id: organizationId, name: "Declared keys", slug: organizationId, createdAt: new Date() });
  const [serviceUser] = await fixture.db.insert(schema.serviceUsers).values({ organizationId, name: "Deployer", role: "creator" }).returning();
  const path = `/api/v1/platform/organizations/${organizationId}/service-users/${serviceUser!.id}/credentials`;
  const secret = `lsa_${"a".repeat(32)}${"B".repeat(32)}`;
  const put = async (body: Record<string, unknown>) => {
    const response = await fixture.request(path, body, cookie, "PUT");
    return { status: response.status, body: await response.json() as { credential: { id: string } | null; created: boolean; secretReplaced: boolean; updated: string[] } };
  };
  expect((await put({ name: "ci", secret: "lsa_short" })).status).toBe(400);
  expect((await put({ name: "ci", secret, dryRun: true })).body).toEqual({ credential: null, created: true, secretReplaced: false, updated: [] });
  expect(await admission(secret)).toBeNull();
  const created = await put({ name: "ci", secret });
  expect(created.body).toMatchObject({ created: true, secretReplaced: false, updated: [] });
  expect((await admission(secret))?.auth).toMatchObject({ principal: { kind: "serviceUser", id: serviceUser!.id }, credential: { id: created.body.credential!.id } });
  expect((await put({ name: "ci", secret })).body).toMatchObject({ created: false, secretReplaced: false, updated: [] });
  const rotated = `lsa_${"c".repeat(64)}`;
  expect((await put({ name: "ci", secret: rotated, restrictions: { readOnly: true }, dryRun: true })).body).toMatchObject({ secretReplaced: true, updated: ["restrictions"] });
  expect(await admission(secret)).not.toBeNull();
  const changed = await put({ name: "ci", secret: rotated, restrictions: { readOnly: true } });
  expect(changed.body).toMatchObject({ credential: { id: created.body.credential!.id }, secretReplaced: true, updated: ["restrictions"] });
  expect(await admission(secret)).toBeNull();
  expect((await admission(rotated))?.auth.credential.restrictions).toEqual({ readOnly: true });
  // A secret serves one key.
  expect((await put({ name: "other", secret: rotated })).status).toBe(409);
});

it("lets only a key created to declare service-user keys declare them, within its restrictions", async () => {
  const organizationId = crypto.randomUUID();
  await fixture.db.insert(schema.organization).values({ id: organizationId, name: "Pipeline", slug: organizationId, createdAt: new Date() });
  const [serviceUser] = await fixture.db.insert(schema.serviceUsers).values({ organizationId, name: "Deployer", role: "creator" }).returning();
  const path = `/api/v1/platform/organizations/${organizationId}/service-users/${serviceUser!.id}/credentials`;
  const keyWith = async (restrictions: Record<string, unknown>) =>
    (await (await fixture.request("/api/v1/platform/credentials", { name: "Pipeline", restrictions }, cookie)).json() as { secret: string }).secret;
  const putWith = (key: string, secret: string) => fixture.app.request(new Request(`${fixture.env.baseUrl}${path}`, {
    method: "PUT", headers: { authorization: `Bearer ${key}`, "content-type": "application/json" }, body: JSON.stringify({ name: "ci", secret }),
  }));
  const declared = `lsa_${"d".repeat(64)}`;
  // An ordinary key mints nothing.
  expect((await putWith(await keyWith({}), declared)).status).toBe(403);
  // Nor does a flagged key that is read-only or limited to another organization.
  expect((await putWith(await keyWith({ allowServiceUserKeys: true, readOnly: true }), declared)).status).toBe(403);
  expect((await putWith(await keyWith({ allowServiceUserKeys: true, organizationIds: ["elsewhere"] }), declared)).status).toBe(403);
  const pipeline = await keyWith({ allowServiceUserKeys: true });
  const response = await putWith(pipeline, declared);
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject({ created: true });
  expect((await admission(declared))?.auth).toMatchObject({ principal: { kind: "serviceUser", id: serviceUser!.id } });
  // The flag declares keys only; other key management stays with people.
  const listed = await fixture.app.request(new Request(`${fixture.env.baseUrl}${path}`, { headers: { authorization: `Bearer ${pipeline}` } }));
  expect(listed.status).toBe(403);
  const audits = await fixture.db.select().from(schema.identityAudit).where(eq(schema.identityAudit.action, "credential.put"));
  expect(audits.some(audit => (audit.details as Record<string, unknown>).principalId === serviceUser!.id
    && (audit.details as Record<string, unknown>).credentialKind === "personalKey")).toBe(true);
});
