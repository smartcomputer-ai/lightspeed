import { createHash } from "node:crypto";
import { afterAll, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { exportJWK, generateKeyPair, SignJWT, type JWTPayload } from "jose";
import { eq } from "drizzle-orm";
import { schema } from "@lightspeed/platform-db";
import { identityFixture, identityEnv, responseCookies } from "../test/identity-fixture.js";
import { companyProviderId } from "./company-identity.js";
import { createAuth } from "./auth.js";

const issuer = identityEnv.oidc!.issuer;
const tokens = new Map<string, { nonce: string; challenge: string; claims: JWTPayload; missingToken?: boolean; wrongSignature?: boolean; accessClaims?: JWTPayload; wrongAccessSignature?: boolean }>();
let keys: Awaited<ReturnType<typeof generateKeyPair>>;
let wrongKeys: Awaited<ReturnType<typeof generateKeyPair>>;
let fixture: Awaited<ReturnType<typeof identityFixture>>;
let fetchMock: ReturnType<typeof vi.fn>;

beforeAll(async () => {
  keys = await generateKeyPair("RS256");
  wrongKeys = await generateKeyPair("RS256");
  const jwk = { ...await exportJWK(keys.publicKey), alg: "RS256", kid: "test" };
  fetchMock = vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
    const url = new URL(input instanceof Request ? input.url : input);
    if (url.origin !== issuer) throw new Error(`Unexpected outbound request: ${url.origin}`);
    if (url.pathname.endsWith("openid-configuration")) return Response.json({
      issuer, authorization_endpoint: `${issuer}/authorize`, token_endpoint: `${issuer}/token`, jwks_uri: `${issuer}/jwks`,
      userinfo_endpoint: `${issuer}/userinfo`, id_token_signing_alg_values_supported: ["RS256"],
    });
    if (url.pathname === "/jwks") return Response.json({ keys: [jwk] });
    if (url.pathname === "/userinfo") return Response.json({ sub: "subject-1" });
    if (url.pathname !== "/token") throw new Error("Unexpected provider endpoint");
    const body = new URLSearchParams(String(init?.body));
    const attempt = tokens.get(body.get("code")!);
    if (!attempt) return Response.json({ error: "invalid_grant" }, { status: 400 });
    tokens.delete(body.get("code")!);
    expect(createHash("sha256").update(body.get("code_verifier")!).digest("base64url")).toBe(attempt.challenge);
    const claims = { iss: issuer, aud: "lightspeed", sub: "subject-1", email: "user@example.test", name: "Company User",
      iat: Math.floor(Date.now() / 1000), exp: Math.floor(Date.now() / 1000) + 300,
      groups: ["lightspeed-users"], nonce: attempt.nonce, ...attempt.claims };
    const idToken = await new SignJWT(claims).setProtectedHeader({ alg: "RS256", kid: "test" })
      .sign(attempt.wrongSignature ? wrongKeys.privateKey : keys.privateKey);
    const accessToken = attempt.accessClaims ? await new SignJWT({ iss: issuer, aud: "urn:lightspeed", sub: "subject-1",
      iat: Math.floor(Date.now() / 1000), exp: Math.floor(Date.now() / 1000) + 300, ...attempt.accessClaims })
      .setProtectedHeader({ alg: "RS256", kid: "test" }).sign(attempt.wrongAccessSignature ? wrongKeys.privateKey : keys.privateKey) : "synthetic-provider-access-secret";
    return Response.json({ token_type: "Bearer", access_token: accessToken, refresh_token: "synthetic-refresh-secret",
      expires_in: 300, ...(attempt.missingToken ? {} : { id_token: idToken }) });
  });
  vi.stubGlobal("fetch", fetchMock);
  fixture = await identityFixture();
}, 30000);

afterAll(async () => { await fixture?.postgres.close(); vi.unstubAllGlobals(); });
beforeEach(async () => {
  await fixture.postgres.exec('DELETE FROM "session"; DELETE FROM "identity_audit"; DELETE FROM "user" WHERE identity_source = \'company\';');
  tokens.clear();
  fetchMock.mockClear();
});

async function login(claims: JWTPayload = {}, options: { missingToken?: boolean; wrongSignature?: boolean; state?: string; accessClaims?: JWTPayload; wrongAccessSignature?: boolean } = {}, target = fixture) {
  const start = await target.request("/api/auth/sign-in/social", {
    provider: companyProviderId(target.env.oidc!), callbackURL: `${target.env.baseUrl}/app/`, errorCallbackURL: `${target.env.baseUrl}/app/login`,
    additionalParams: { resource: "attacker-resource" }, scopes: ["offline_access"],
  });
  expect(start.status).toBe(200);
  const url = new URL(((await start.json()) as { url: string }).url);
  expect(url.searchParams.get("resource")).toBe("urn:lightspeed");
  expect(url.searchParams.get("scope")).not.toContain("offline_access");
  expect(url.searchParams.get("code_challenge_method")).toBe("S256");
  expect(url.searchParams.get("nonce")).toBeTruthy();
  const code = crypto.randomUUID();
  tokens.set(code, { nonce: url.searchParams.get("nonce")!, challenge: url.searchParams.get("code_challenge")!, claims, ...options });
  const callback = new URL(url.searchParams.get("redirect_uri")!);
  callback.searchParams.set("code", code);
  callback.searchParams.set("state", options.state ?? url.searchParams.get("state")!);
  const response = await target.request(callback.pathname + callback.search, undefined, responseCookies(start));
  const cookie = responseCookies(response);
  return { response, cookie, callback, initCookie: responseCookies(start) };
}

async function emergency() {
  const response = await fixture.request("/api/auth/sign-in/email", { email: identityEnv.adminEmail, password: identityEnv.adminPassword });
  expect(response.status).toBe(200);
  return responseCookies(response);
}
async function companyUser() {
  return (await fixture.db.select().from(schema.user).where(eq(schema.user.identitySource, "company")))[0]!;
}

it.each([
  ["enabled", identityEnv.oidc, "emergency.sign_in"],
  ["disabled", null, "password.sign_in"],
] as const)("audits bootstrap admin password sign-in with SSO %s", async (_name, oidc, action) => {
  const auth = createAuth(fixture.db, { ...identityEnv, oidc });
  const response = await auth.handler(new Request(`${identityEnv.baseUrl}/api/auth/sign-in/email`, {
    method: "POST", headers: { "content-type": "application/json", origin: identityEnv.baseUrl },
    body: JSON.stringify({ email: identityEnv.adminEmail, password: identityEnv.adminPassword }),
  }));
  expect(response.status).toBe(200);
  const admin = (await fixture.db.select().from(schema.user).where(eq(schema.user.email, identityEnv.adminEmail!)))[0]!;
  expect(admin.emergencyAdmin).toBe(true);
  expect(await fixture.db.select().from(schema.identityAudit)).toEqual([
    expect.objectContaining({ actorId: admin.id, targetId: admin.id, action, outcome: "success" }),
  ]);
});

it.each([
  [["lightspeed-users"], "user"], [["lightspeed-admins"], "admin"], [["lightspeed-users", "lightspeed-admins"], "admin"],
])("admits %j with local platform role %s and no universe membership", async (groups, role) => {
  const { cookie } = await login({ groups });
  const me = await fixture.request("/api/v1/me", undefined, cookie);
  expect(me.status).toBe(200);
  expect(((await me.json()) as { user: unknown }).user).toMatchObject({ role, identitySource: "company", companyAdmitted: true });
  expect(await (await fixture.request("/api/v1/universes", undefined, cookie)).json()).toEqual([]);
  const account = (await fixture.db.select().from(schema.account).where(eq(schema.account.userId, (await companyUser()).id)))[0]!;
  expect([account.accessToken, account.refreshToken, account.idToken]).toEqual([null, null, null]);
  const audit = JSON.stringify(await fixture.db.select().from(schema.identityAudit));
  expect(audit).not.toContain("secret");
});

it.each([
  ["no entitlement", { groups: [] }], ["missing entitlement", { groups: undefined }], ["malformed entitlement", { groups: [42] }],
  ["wrong issuer", { iss: "https://other.example.test" }], ["wrong audience", { aud: "another-app" }],
  ["expired token", { exp: 1 }], ["missing expiry", { exp: undefined }], ["missing subject", { sub: undefined }],
  ["wrong nonce", { nonce: "wrong" }], ["missing nonce", { nonce: undefined }], ["missing email", { email: undefined }],
])("refuses %s before creating a company user or session", async (_name, claims) => {
  const { cookie } = await login(claims as JWTPayload);
  expect((await fixture.request("/api/v1/me", undefined, cookie)).status).toBe(401);
  expect(await companyUser()).toBeUndefined();
});

it.each([{ missingToken: true }, { wrongSignature: true }, { state: "unrelated-state" }])("refuses invalid protocol response %j", async (options) => {
  await login({}, options);
  expect(await companyUser()).toBeUndefined();
});

it("preserves identity across email changes, refuses collisions and never links local accounts", async () => {
  await login();
  const original = await companyUser();
  await login({ email: "changed@example.test" });
  expect(await companyUser()).toMatchObject({ id: original.id, email: "changed@example.test" });
  const collision = await login({ sub: "different-subject", email: "changed@example.test" });
  expect((await fixture.request("/api/v1/me", undefined, collision.cookie)).status).toBe(401);
  const localCollision = await login({ sub: "local-collision", email: identityEnv.adminEmail! });
  expect((await fixture.request("/api/v1/me", undefined, localCollision.cookie)).status).toBe(401);
});

it("demotes existing sessions and revokes all of them when company admission disappears", async () => {
  const first = await login({ groups: ["lightspeed-admins"] });
  const second = await login({ groups: ["lightspeed-users"] });
  expect(((await (await fixture.request("/api/v1/me", undefined, first.cookie)).json()) as { user: { role: string } }).user.role).toBe("user");
  const denied = await login({ groups: [] });
  for (const { cookie } of [first, second, denied]) expect((await fixture.request("/api/v1/me", undefined, cookie)).status).toBe(401);
  expect(await companyUser()).toMatchObject({ role: "user", companyAdmitted: false });
  const restored = await login();
  expect((await fixture.request("/api/v1/me", undefined, restored.cookie)).status).toBe(200);
  expect((await fixture.request("/api/v1/me", undefined, first.cookie)).status).toBe(401);
});

it("bounds sessions, does not slide with activity, and rejects stale bearer and cookie sessions", async () => {
  const { cookie } = await login();
  const user = await companyUser();
  const [session] = await fixture.db.select().from(schema.session).where(eq(schema.session.userId, user.id));
  expect(session!.expiresAt.getTime() - user.providerCheckedAt!.getTime()).toBeLessThanOrEqual(28800000);
  await fixture.request("/api/v1/me", undefined, cookie);
  const [again] = await fixture.db.select().from(schema.session).where(eq(schema.session.id, session!.id));
  expect(again!.expiresAt).toEqual(session!.expiresAt);
  const bearer = await fixture.app.request(`${identityEnv.baseUrl}/api/v1/me`, { headers: { authorization: `Bearer ${session!.token}` } });
  expect(bearer.status).toBe(200);
  await fixture.db.update(schema.session).set({ expiresAt: new Date(0) }).where(eq(schema.session.id, session!.id));
  expect((await fixture.request("/api/v1/me", undefined, cookie)).status).toBe(401);
  expect((await fixture.app.request(`${identityEnv.baseUrl}/api/v1/me`, { headers: { authorization: `Bearer ${session!.token}` } })).status).toBe(401);
});

it("enforces suspension across auth routes, reauthentication and reinstatement without reviving old sessions", async () => {
  const adminCookie = await emergency();
  const { cookie } = await login();
  const user = await companyUser();
  expect((await fixture.request("/api/auth/admin/ban-user", { userId: user.id, banExpiresIn: 1 }, adminCookie)).status).toBe(200);
  expect((await fixture.request("/api/auth/update-user", { name: "changed" }, cookie)).status).toBe(401);
  expect(await (await fixture.request("/api/auth/get-session", undefined, cookie)).json()).toBeNull();
  expect((await fixture.request("/api/v1/me", undefined, (await login()).cookie)).status).toBe(401);
  await fixture.request("/api/auth/admin/unban-user", { userId: user.id }, adminCookie);
  expect((await fixture.request("/api/v1/me", undefined, cookie)).status).toBe(401);
  expect((await fixture.request("/api/v1/me", undefined, (await login()).cookie)).status).toBe(200);
  expect((await fixture.db.select().from(schema.identityAudit)).map((r) => r.action)).toEqual(expect.arrayContaining(["emergency.sign_in", "user.suspend", "user.reinstate"]));
});

it("blocks password, profile/role injection, linking and alternate login paths for company users", async () => {
  const adminCookie = await emergency();
  const { cookie } = await login({ groups: ["lightspeed-admins"] });
  const user = await companyUser();
  for (const [path, body] of [
    ["/admin/set-user-password", { userId: user.id, newPassword: "new-test-password" }],
    ["/admin/update-user", { userId: user.id, data: { role: "admin" } }],
    ["/admin/update-user", { userId: user.id, data: { emergencyAdmin: true } }],
    ["/admin/update-user", { userId: user.id, data: { banned: false } }],
  ] as const) expect((await fixture.request(`/api/auth${path}`, body, adminCookie)).status).toBe(403);
  expect((await fixture.request("/api/auth/change-password", { currentPassword: "x", newPassword: "new-test-password" }, cookie)).status).toBe(403);
  expect((await fixture.request("/api/auth/sign-in/email", { email: user.email, password: "new-test-password" })).status).toBe(401);
  for (const path of ["/link-social", "/set-password", "/reset-password", "/admin/impersonate-user", "/organization/add-member"]) {
    expect((await fixture.request(`/api/auth${path}`, {}, cookie)).status).toBe(404);
  }
  expect((await fixture.request("/api/auth/sign-in/social", { provider: companyProviderId(identityEnv.oidc!), idToken: { token: "anything" } })).status).toBe(403);
  expect((await fixture.request("/api/auth/sign-in/social", { provider: "github" })).status).toBe(403);
});

it("validates JWT access-token entitlements against their own audience and the ID-token subject", async () => {
  const target = await identityFixture({ ...identityEnv, oidc: { ...identityEnv.oidc!, claimsToken: "access", audience: "urn:lightspeed" } });
  try {
    for (const accessClaims of [
      { groups: ["lightspeed-users"], aud: "wrong" },
      { groups: ["lightspeed-users"], iss: "https://wrong.example.test" },
      { groups: ["lightspeed-users"], sub: "someone-else" },
      { groups: ["lightspeed-users"], exp: 1 }, { groups: [] },
      { groups: ["lightspeed-users"], iat: Math.floor(Date.now() / 1000) + 600 },
    ]) {
      const denied = await login({}, { accessClaims }, target);
      expect((await target.request("/api/v1/me", undefined, denied.cookie)).status).toBe(401);
    }
    const signature = await login({}, { accessClaims: { groups: ["lightspeed-users"] }, wrongAccessSignature: true }, target);
    expect((await target.request("/api/v1/me", undefined, signature.cookie)).status).toBe(401);
    const opaque = await login({}, {}, target);
    expect((await target.request("/api/v1/me", undefined, opaque.cookie)).status).toBe(401);
    const allowed = await login({ groups: [] }, { accessClaims: { groups: ["lightspeed-admins"] } }, target);
    expect((await target.request("/api/v1/me", undefined, allowed.cookie)).status).toBe(200);
    expect((await target.db.select().from(schema.user).where(eq(schema.user.identitySource, "company")))[0]?.role).toBe("admin");
  } finally { await target.postgres.close(); }
}, 30000);

it("keeps universe roles local, enforces their edits on existing sessions, and retains durable audit records", async () => {
  const adminCookie = await emergency();
  const { cookie } = await login();
  const company = await companyUser();
  const org = crypto.randomUUID();
  await fixture.db.insert(schema.organization).values({ id: org, name: "Local roles", slug: org, createdAt: new Date() });
  const [universe] = await fixture.db.insert(schema.universes).values({ organizationId: org, name: "Local roles", lightspeedUniverseId: crypto.randomUUID() }).returning();
  const base = `/api/v1/universes/${universe!.id}`;
  const added = await fixture.request(`${base}/members`, { userId: company.id, role: "viewer" }, adminCookie);
  expect(added.status).toBe(201);
  const member = await added.json() as { id: string };
  const renewed = await login();
  expect((await fixture.db.select().from(schema.member).where(eq(schema.member.id, member.id)))[0]?.role).toBe("viewer");
  expect((await fixture.request(`${base}/members`, { userId: company.id, role: "admin" }, cookie)).status).toBe(403);
  expect((await fixture.request(`${base}/members/${member.id}`, { role: "operator" }, adminCookie, "PATCH")).status).toBe(200);
  for (const session of [cookie, renewed.cookie]) {
    const listed = await (await fixture.request("/api/v1/universes", undefined, session)).json() as Array<{ role: string }>;
    expect(listed[0]?.role).toBe("operator");
  }
  expect((await fixture.request(`${base}/members/${member.id}`, undefined, adminCookie, "DELETE")).status).toBe(200);
  expect(await (await fixture.request("/api/v1/universes", undefined, (await login()).cookie)).json()).toEqual([]);
  expect((await fixture.request("/api/v1/admin/audit", undefined, cookie)).status).toBe(403);
  await fixture.db.delete(schema.user).where(eq(schema.user.id, company.id));
  const events = await (await fixture.request("/api/v1/admin/audit", undefined, adminCookie)).json() as Array<{ targetId: string; action: string }>;
  expect(events.filter((event) => event.targetId === company.id).map((event) => event.action)).toEqual(expect.arrayContaining(["member.add", "member.role", "member.remove"]));
  await fixture.db.delete(schema.organization).where(eq(schema.organization.id, org));
});

it("keeps local password login and admin provisioning available without OIDC", async () => {
  const target = await identityFixture({ ...identityEnv, oidc: null });
  try {
    const signedIn = await target.request("/api/auth/sign-in/email", { email: identityEnv.adminEmail, password: identityEnv.adminPassword });
    expect(signedIn.status).toBe(200);
    const created = await target.request("/api/auth/admin/create-user", { name: "Local user", email: "local@example.test", password: "local-test-password", role: "user" }, responseCookies(signedIn));
    expect(created.status).toBe(200);
    const local = await target.request("/api/auth/sign-in/email", { email: "local@example.test", password: "local-test-password" });
    expect((await target.request("/api/v1/me", undefined, responseCookies(local))).status).toBe(200);
  } finally { await target.postgres.close(); }
}, 30000);

it("lets emergency admins change their own password and rejects callback replay", async () => {
  const changed = await fixture.request("/api/auth/change-password", { currentPassword: identityEnv.adminPassword, newPassword: "replacement-emergency-password", revokeOtherSessions: true }, await emergency());
  expect(changed.status).toBe(200);
  const newCookie = responseCookies(changed);
  expect((await fixture.request("/api/v1/me", undefined, newCookie)).status).toBe(200);
  expect((await fixture.request("/api/auth/change-password", { currentPassword: "replacement-emergency-password", newPassword: identityEnv.adminPassword }, newCookie)).status).toBe(200);
  const attempt = await login();
  const replay = await fixture.request(attempt.callback.pathname + attempt.callback.search, undefined, attempt.initCookie);
  expect((await fixture.request("/api/v1/me", undefined, responseCookies(replay))).status).toBe(401);
});

it("keeps emergency access working during provider failure and can turn passwords off", async () => {
  const existing = await login();
  fetchMock.mockImplementation(async () => { throw new Error("provider unavailable"); });
  expect((await fixture.request("/api/v1/me", undefined, existing.cookie)).status).toBe(200);
  expect((await fixture.request("/api/v1/me", undefined, await emergency())).status).toBe(200);
  const disabled = createAuth(fixture.db, { ...identityEnv, passwordSignIn: "off" });
  const response = await disabled.handler(new Request(`${identityEnv.baseUrl}/api/auth/sign-in/email`, {
    method: "POST", headers: { "content-type": "application/json", origin: identityEnv.baseUrl },
    body: JSON.stringify({ email: identityEnv.adminEmail, password: identityEnv.adminPassword }),
  }));
  expect(response.status).toBe(401);
});
