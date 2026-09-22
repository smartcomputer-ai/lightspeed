import { LightspeedClient } from "@lightspeed-ai/agent-client";
import { handleOAuthUserInfo } from "better-auth/oauth2";
// Invoked by the ignored Rust Platform integration suite with disposable services.
import assert from "node:assert/strict";
import pg from "pg";
import { createDb, migrateDb } from "@lightspeed/platform-db";
import { buildApp } from "../src/api.js";
import { createAuth, type Auth } from "../src/auth.js";
import { bootstrapAdmin } from "../src/bootstrap.js";
import type { ServerEnv } from "../src/env.js";
import { provisioningClient } from "../src/runtime-client.js";

function required(name: string) { const v = process.env[name]; if (!v) throw new Error(`${name} required`); return v; }
const base = required("LIGHTSPEED_TEST_POSTGRES_URL");
const database = `platform_identity_${crypto.randomUUID().replaceAll("-", "")}`;
const admin = new pg.Client({ connectionString: base });
await admin.connect();
await admin.query(`CREATE DATABASE "${database}"`);
const url = new URL(base); url.pathname = `/${database}`;
const handle = createDb(url.toString());
const env: ServerEnv = {
  databaseUrl: url.toString(), authSecret: "disposable-platform-integration-secret-12345", baseUrl: "http://localhost:3100", trustedOrigins: [], port: 3100,
  adminEmail: "admin@identity.test", adminPassword: "disposable-password", adminPrincipalId: required("LIGHTSPEED_TEST_ADMIN_PRINCIPAL"),
  lightspeedApiUrl: required("LIGHTSPEED_TEST_API_URL"), lightspeedApiKey: required("LIGHTSPEED_TEST_PLATFORM_KEY"), github: null,
  configuratorMcpUrl: "https://configurator.example.test/mcp", configuratorMcpAllowPrivateNetwork: false, configuratorMcpInternalTrustedHeader: false, channelsHealthUrls: [], devEnvdEndpoint: null,
};
try {
  await migrateDb(handle);
  await bootstrapAdmin(handle.db, env);
  const auth = createAuth(handle.db, env);
  // External sign-up goes through the same function the OAuth callback runs,
  // so it exercises input validation of the user fields, the creation hook
  // that provisions the core principal, and the account-linking policy.
  const authContext = await auth.$context;
  const oauthContext = { context: authContext } as unknown as Parameters<typeof handleOAuthUserInfo>[0];
  const providerAccount = (accountId: string) => ({ providerId: "github", accountId, accessToken: "disposable", refreshToken: undefined, idToken: undefined, accessTokenExpiresAt: undefined, refreshTokenExpiresAt: undefined, scope: "read:user" });
  const signUp = await handleOAuthUserInfo(oauthContext, {
    userInfo: { id: "external", name: "External account", email: "external@identity.test", emailVerified: true, image: null },
    account: providerAccount("gh-external"),
  });
  assert.equal(signUp.error, null, signUp.error ?? undefined);
  assert.equal(signUp.isRegister, true);
  const external = signUp.data!.user as Auth["$Infer"]["Session"]["user"];
  assert.ok(external.corePrincipalId);
  const externalPrincipal = external.corePrincipalId;
  const adapter = authContext.internalAdapter;
  await adapter.updateUser(external.id, { name: "External account renamed" });
  const externalAfter = await adapter.findUserById(external.id) as Auth["$Infer"]["Session"]["user"] | null;
  assert.equal(externalAfter?.corePrincipalId, externalPrincipal);
  // A provider account carrying a local user's verified e-mail is refused, never linked.
  const linkAttempt = await handleOAuthUserInfo(oauthContext, {
    userInfo: { id: "impostor", name: "Impostor", email: env.adminEmail!, emailVerified: true, image: null },
    account: providerAccount("gh-impostor"),
  });
  assert.equal(linkAttempt.error, "account not linked");
  assert.equal(linkAttempt.data, null);
  const app = buildApp({ ...handle, auth, env });
  let checks = 0;
  async function call(method: string, path: string, cookie = "", body?: unknown, expected = 200, extra: Record<string, string> = {}) {
    const response = await app.request(`${env.baseUrl}${path}`, { method, headers: { cookie, origin: env.baseUrl, "content-type": "application/json", ...extra }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
    const text = await response.text();
    const value = text ? JSON.parse(text) : null;
    assert.equal(response.status, expected, `${method} ${path}: ${JSON.stringify(value)}`); checks++;
    return value;
  }
  async function login(email: string) {
    const response = await app.request(`${env.baseUrl}/api/auth/sign-in/email`, { method: "POST", headers: { origin: env.baseUrl, "content-type": "application/json" }, body: JSON.stringify({ email, password: env.adminPassword }) });
    assert.equal(response.status, 200, await response.clone().text()); checks++;
    const cookie = response.headers.getSetCookie().map((s) => s.split(";")[0]).join("; ");
    assert.ok(cookie); return cookie;
  }
  const root = await login(env.adminEmail!);
  const me = await call("GET", "/api/v1/me", root);
  assert.equal(me.user.corePrincipalId, env.adminPrincipalId);
  const alice = (await call("POST", "/api/v1/admin/users", root, { name: "Alice", email: "alice@identity.test", password: env.adminPassword, role: "user" }, 201)).user;
  const bob = (await call("POST", "/api/v1/admin/users", root, { name: "Bob", email: "bob@identity.test", password: env.adminPassword, role: "user" }, 201)).user;
  assert.notEqual(alice.id, alice.corePrincipalId);
  const [a, b] = await Promise.all([login(alice.email), login(bob.email)]);
  await call("POST", "/api/v1/admin/identity", a, { operation: "create_group", id: crypto.randomUUID(), displayName: "Escalate" }, 403);
  const created = await call("POST", "/api/v1/universes", root, { name: "Created identity test" }, 201);
  const createdMembers = await call("GET", `/api/v1/universes/${created.id}/members`, root);
  assert.equal(createdMembers[0].userId, me.user.id);
  await call("DELETE", `/api/v1/universes/${created.id}/members/${createdMembers[0].id}`, root, undefined, 409);
  const fixtureId = required("LIGHTSPEED_TEST_UNIVERSE_ID");
  await call("POST", "/api/v1/admin/identity", root, { operation: "assign_role", assignment: { scope: { kind: "universe", universeId: fixtureId }, subject: { kind: "principal", id: env.adminPrincipalId }, role: "admin" } });
  const universe = await call("POST", "/api/v1/universes/adopt", root, { name: "Identity test", lightspeedUniverseId: fixtureId }, 201);
  const prefix = `/api/v1/universes/${universe.id}`;
  assert.deepEqual(await call("GET", "/api/v1/universes", a), []);
  await call("GET", `${prefix}/sessions`, a, undefined, 404);
  await call("POST", `${prefix}/members`, root, { userId: alice.id, role: "contributor" }, 201);
  const groupId = crypto.randomUUID();
  const change = (body: unknown, expected = 200) => call("POST", "/api/v1/admin/identity", root, body, expected);
  await change({ operation: "create_group", id: groupId, displayName: "Readers" });
  await change({ operation: "put_membership", membership: { groupId, principalId: bob.corePrincipalId } });
  await call("POST", `${prefix}/members`, root, { groupId, role: "viewer" }, 201);
  assert.equal((await call("GET", "/api/v1/universes", b))[0].role, "viewer");
  const personalKey = await call("POST", `${prefix}/api-keys`, a, { principalId: alice.corePrincipalId, displayName: "Scoped test" }, 201);
  const direct = new LightspeedClient({ endpoint: env.lightspeedApiUrl!, headers: { authorization: `Bearer ${personalKey.secret}` } });
  const own = await direct.call("deployment/identity/self", { scope: { kind: "universe", universeId: fixtureId } });
  assert.equal(own.result.access.principal.id, alice.corePrincipalId);
  assert.deepEqual(own.result.universes.map((u) => u.scope), [{ kind: "universe", universeId: fixtureId }]);
  await assert.rejects(direct.call("deployment/identity/self", { scope: { kind: "deployment" } }));
  await assert.rejects(direct.call("deployment/identity/directory", { scope: { kind: "universe", universeId: fixtureId } }));
  await assert.rejects(direct.call("deployment/identity/apply", { operation: "create_group", id: crypto.randomUUID(), displayName: "Forbidden" }));
  checks += 4;
  await call("PUT", `${prefix}/profiles/alice-profile`, a, { profileId: "alice-profile" });
  await call("PUT", `${prefix}/profiles/viewer-profile`, b, { profileId: "viewer-profile" }, 403);
  await call("POST", `${prefix}/members`, a, { userId: bob.id, role: "admin" }, 403);
  await call("POST", `${prefix}/members`, root, { userId: bob.id, role: "contributor" }, 201);
  await call("PUT", `${prefix}/profiles/alice-profile`, b, { profileId: "alice-profile" }, 403);
  const [sa, sb] = await Promise.all([
    call("POST", `${prefix}/sessions`, a, { profile: { kind: "named", profileId: "alice-profile" } }),
    call("POST", `${prefix}/sessions`, b, { profile: { kind: "named", profileId: "alice-profile" } }),
  ]);
  await call("GET", `${prefix}/sessions/${sa.id}`, b);
  await call("POST", `${prefix}/sessions/${sa.id}/messages`, b, { text: "not mine", submissionId: "spoof" }, 403, { "x-lightspeed-principal": `user:${alice.corePrincipalId}` });
  await call("POST", `${prefix}/sessions/${sa.id}/messages`, root, { text: "admin cannot steer", submissionId: "admin-spoof" }, 403);
  // Local login updates cannot rebind a canonical principal or grant roles.
  await call("POST", "/api/auth/update-user", a, { name: "Alice renamed", corePrincipalId: env.adminPrincipalId, role: "admin" }, 400);
  await call("POST", "/api/auth/update-user", a, { name: "Alice renamed", role: "admin" });
  const renamed = await call("GET", "/api/v1/me", a);
  assert.equal(renamed.user.corePrincipalId, alice.corePrincipalId); assert.equal(renamed.user.role, "user");
  await call("POST", `${prefix}/members`, root, { userId: alice.id, role: "admin" }, 201);
  const scopedDirectory = (await direct.call("deployment/identity/directory", { scope: { kind: "universe", universeId: fixtureId } })).result;
  assert.ok(scopedDirectory.roles.every((r) => r.scope.kind === "universe" && r.scope.universeId === fixtureId));
  await assert.rejects(direct.call("deployment/identity/directory", { scope: { kind: "universe", universeId: created.lightspeedUniverseId } }));
  const setup = await call("POST", `${prefix}/setups/configurator/install`, a, {});
  assert.equal(setup.status, "ready");
  const retriedSetup = await call("POST", `${prefix}/setups/configurator/install`, a, {});
  assert.equal(retriedSetup.resources.principalId, setup.resources.principalId);
  checks += 2;
  await call("PATCH", `/api/v1/admin/users/${alice.id}`, root, { email: "alice2@identity.test", role: "admin" });
  assert.equal((await call("GET", "/api/v1/me", a)).user.corePrincipalId, alice.corePrincipalId);
  // Group and direct changes affect the next request with the same login session.
  const members = await call("GET", `${prefix}/members`, root);
  const bobContributor = members.find((m: { userId: string; role: string }) => m.userId === bob.id && m.role === "contributor");
  await call("DELETE", `${prefix}/members/${bobContributor.id}`, root);
  await call("POST", `${prefix}/sessions/${sb.id}/messages`, b, { text: "revoked writer", submissionId: "revoked" }, 403);
  await change({ operation: "remove_membership", membership: { groupId, principalId: bob.corePrincipalId } });
  assert.deepEqual(await call("GET", "/api/v1/universes", b), []);
  await call("GET", `${prefix}/sessions/${sa.id}`, b, undefined, 404);
  // Deployment admin without universe membership may administer, never read content.
  const rootMember = members.find((m: { userId: string; role: string }) => m.userId === me.user.id && m.role === "admin");
  await call("DELETE", `${prefix}/members/${rootMember.id}`, root);
  await call("GET", `${prefix}/sessions/${sa.id}`, root, undefined, 404);
  const bindings = await call("GET", "/api/v1/admin/environment-provider-bindings", root);
  assert.ok(bindings.every((u: { error: unknown }) => u.error === null), "configuration inventory must not require content membership");
  // Core disablement blocks even an existing Better Auth session immediately.
  await change({ operation: "set_principal_status", id: bob.corePrincipalId, status: "disabled" });
  await call("GET", "/api/v1/me", b, undefined, 403);
  await change({ operation: "set_principal_status", id: bob.corePrincipalId, status: "active" });
  await call("GET", "/api/v1/me", b);
  // Password reset also revokes local login sessions.
  await call("POST", `/api/v1/admin/users/${bob.id}/password`, root, { newPassword: "new-disposable-password" });
  await call("GET", "/api/v1/me", b, undefined, 401);
  await call("POST", "/api/auth/admin/create-user", root, { name: "Legacy", email: "legacy@identity.test", password: env.adminPassword }, 404);
  await call("POST", "/api/auth/organization/create", root, { name: "Legacy", slug: "legacy" }, 404);
  // Service has assertion/provisioning capabilities, no borrowed universe access.
  await assert.rejects(provisioningClient(env).call("deployment/universes/list", {})); checks++;
  console.log(JSON.stringify({ checks, alice: alice.corePrincipalId, bob: bob.corePrincipalId, universe: universe.lightspeedUniverseId, sessions: [sa.id, sb.id] }));
} finally {
  await handle.pool.end();
  await admin.query(`DROP DATABASE "${database}" WITH (FORCE)`);
  await admin.end();
}
