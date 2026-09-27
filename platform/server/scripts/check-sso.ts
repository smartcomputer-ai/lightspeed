// Deliberate live check against the repository's loopback-only Keycloak fixture.
// No production provider or credentials are accepted here.
import assert from "node:assert/strict";
import { eq } from "drizzle-orm";
import { schema } from "@lightspeed/platform-db";
import { identityEnv, identityFixture, responseCookies } from "../test/identity-fixture.js";
import { companyProviderId } from "../src/company-identity.js";

const issuer = process.env.LIGHTSPEED_PLATFORM_OIDC_TEST_ISSUER;
if (issuer !== "http://localhost:18090/realms/lightspeed") throw new Error("Set LIGHTSPEED_PLATFORM_OIDC_TEST_ISSUER=http://localhost:18090/realms/lightspeed and start scripts/dev/oidc/compose.yaml");
const config = { ...identityEnv.oidc!, issuer, discoveryUrl: `${issuer}/.well-known/openid-configuration`, clientId: "lightspeed-platform", clientSecret: "lightspeed-local-oidc-secret", resource: undefined };
const fixture = await identityFixture({ ...identityEnv, oidc: config });
type Browser = Map<string, string>;
function cookies(jar: Browser) { return [...jar].map(([key, value]) => `${key}=${value}`).join("; "); }
function absorb(jar: Browser, response: Response) {
  for (const cookie of response.headers.getSetCookie()) {
    const pair = cookie.split(";")[0]!;
    const split = pair.indexOf("=");
    jar.set(pair.slice(0, split), pair.slice(split + 1));
  }
}
async function providerRequest(url: string, jar: Browser, body?: URLSearchParams) {
  const response = await fetch(url, { redirect: "manual", method: body ? "POST" : "GET", body,
    headers: { cookie: cookies(jar), ...(body ? { "content-type": "application/x-www-form-urlencoded", origin: "http://localhost:18090" } : {}) } });
  absorb(jar, response);
  return response;
}
async function login(username: string, jar: Browser) {
  const started = await fixture.request("/api/auth/sign-in/social", { provider: companyProviderId(config), callbackURL: `${identityEnv.baseUrl}/app/`, errorCallbackURL: `${identityEnv.baseUrl}/app/login` });
  assert.equal(started.status, 200);
  let url = ((await started.json()) as { url: string }).url;
  let prompted = false;
  for (let step = 0; step < 10; step++) {
    if (url.startsWith(identityEnv.baseUrl)) {
      const callback = await fixture.request(url, undefined, responseCookies(started));
      const cookie = responseCookies(callback);
      const me = await fixture.request("/api/v1/me", undefined, cookie);
      return { cookie, status: me.status, user: me.ok ? ((await me.json()) as { user: { id: string; role: string } }).user : null, prompted };
    }
    assert.equal(new URL(url).origin, "http://localhost:18090");
    const response = await providerRequest(url, jar);
    const location = response.headers.get("location");
    if (location) { url = new URL(location, url).href; continue; }
    const html = await response.text();
    const action = html.match(/<form[^>]*id="kc-form-login"[^>]*action="([^"]+)"/)?.[1];
    if (!action || prompted) return { cookie: "", status: 401, user: null, prompted };
    prompted = true;
    const submitted = await providerRequest(action.replaceAll("&amp;", "&"), jar, new URLSearchParams({ username, password: "lightspeed-local-password", credentialId: "" }));
    const next = submitted.headers.get("location");
    if (!next) return { cookie: "", status: 401, user: null, prompted };
    url = new URL(next, url).href;
  }
  throw new Error("Unexpected provider redirect loop");
}
const tokenResponse = await fetch("http://localhost:18090/realms/master/protocol/openid-connect/token", {
  method: "POST", body: new URLSearchParams({ client_id: "admin-cli", grant_type: "password", username: "admin", password: "lightspeed-local-admin" }),
});
assert.equal(tokenResponse.status, 200);
const adminToken = ((await tokenResponse.json()) as { access_token: string }).access_token;
async function admin(path: string, method = "GET", body?: unknown) {
  const response = await fetch(`http://localhost:18090/admin/realms/lightspeed/${path}`, {
    method, headers: { authorization: `Bearer ${adminToken}`, "content-type": "application/json" }, body: body ? JSON.stringify(body) : undefined,
  });
  assert(response.ok, `Keycloak administration failed: ${method} ${path} (${response.status})`);
  return response.status === 204 ? null : response.json();
}
const users = await admin("users") as Array<{ id: string; username: string }>;
const groups = await admin("groups") as Array<{ id: string; name: string }>;
const adminId = users.find((user) => user.username === "sso-admin")!.id;
const userId = users.find((user) => user.username === "sso-user")!.id;
const usersGroup = groups.find((group) => group.name === "lightspeed-users")!.id;
const adminsGroup = groups.find((group) => group.name === "lightspeed-admins")!.id;
try {
  const userBrowser: Browser = new Map();
  const first = await login("sso-user", userBrowser);
  assert.equal(first.status, 200); assert.equal(first.user?.role, "user");
  assert.equal(first.prompted, true);
  const renewed = await login("sso-user", userBrowser);
  assert.equal(renewed.status, 200); assert.equal(renewed.prompted, false);
  assert.equal((await login("sso-denied", new Map())).status, 401);
  const adminBrowser: Browser = new Map();
  const elevated = await login("sso-admin", adminBrowser);
  assert.equal(elevated.user?.role, "admin");
  await admin(`users/${adminId}/groups/${usersGroup}`, "PUT");
  await admin(`users/${adminId}/groups/${adminsGroup}`, "DELETE");
  const demoted = await login("sso-admin", adminBrowser);
  assert.equal(demoted.user?.role, "user"); assert.equal(demoted.prompted, false);
  assert.equal(((await (await fixture.request("/api/v1/me", undefined, elevated.cookie)).json()) as { user: { role: string } }).user.role, "user");
  await admin(`users/${adminId}/groups/${usersGroup}`, "DELETE");
  assert.equal((await login("sso-admin", adminBrowser)).status, 401);
  assert.equal((await fixture.request("/api/v1/me", undefined, elevated.cookie)).status, 401);
  await admin(`users/${userId}`, "PUT", { enabled: false });
  assert.equal((await login("sso-user", userBrowser)).status, 401);
  // A provider-side denial alone does not tell Platform which sessions to
  // revoke. Their original bound continues to apply.
  assert.equal((await fixture.request("/api/v1/me", undefined, first.cookie)).status, 200);
  await fixture.db.update(schema.session).set({ expiresAt: new Date(0) }).where(eq(schema.session.userId, first.user!.id));
  assert.equal((await fixture.request("/api/v1/me", undefined, first.cookie)).status, 401);
  const emergency = await fixture.request("/api/auth/sign-in/email", { email: identityEnv.adminEmail, password: identityEnv.adminPassword });
  assert.equal(emergency.status, 200);
  console.log("Keycloak: code + PKCE login, silent renewal, ordinary/admin admission, denial, demotion, removal, disabled-account renewal, absolute expiry and emergency access passed.");
} finally {
  await admin(`users/${userId}`, "PUT", { enabled: true });
  await admin(`users/${adminId}/groups/${adminsGroup}`, "PUT");
  await admin(`users/${adminId}/groups/${usersGroup}`, "DELETE");
  await fixture.postgres.close();
}
