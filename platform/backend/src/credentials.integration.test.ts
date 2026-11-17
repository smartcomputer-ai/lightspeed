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
