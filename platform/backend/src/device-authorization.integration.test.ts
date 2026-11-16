import { afterAll, beforeAll, beforeEach, expect, it } from "vitest";
import { eq } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { identityFixture, identityEnv, responseCookies } from "../test/identity-fixture.js";

let fixture: Awaited<ReturnType<typeof identityFixture>>;
let cookie: string;
const clientId = "lightspeed-cli";
type Code = { device_code: string; user_code: string; verification_uri: string; verification_uri_complete: string; expires_in: number; interval: number };
const bearer = (token: string, path = "/api/v1/platform/session", method = "GET", body?: unknown, extra?: Record<string, string>) => fixture.app.request(new Request(`${fixture.env.baseUrl}${path}`, {
  method, headers: { authorization: `Bearer ${token}`, ...extra, ...(body ? { "content-type": "application/json" } : {}) }, ...(body ? { body: JSON.stringify(body) } : {}),
}));
const poll = (code: Code) => fixture.request("/api/auth/device/token", { client_id: clientId, device_code: code.device_code, grant_type: "urn:ietf:params:oauth:grant-type:device_code" });
async function start(): Promise<Code> {
  const response = await fixture.request("/api/auth/device/code", { client_id: clientId });
  expect(response.status).toBe(200);
  expect(response.headers.get("cache-control")).toBe("no-store");
  const result = await response.json() as Code;
  expect(result.interval).toBe(5); expect(result.expires_in).toBe(600);
  expect(new URL(result.verification_uri_complete).searchParams.get("user_code")).toBe(result.user_code);
  return result;
}
async function claim(code: Code, browser = cookie) {
  return fixture.request(`/api/auth/device?user_code=${code.user_code}`, undefined, browser);
}
async function approve(code: Code) {
  expect((await claim(code)).status).toBe(200);
  expect((await fixture.request("/api/auth/device/approve", { userCode: code.user_code }, cookie)).status).toBe(200);
}
async function loginCli() {
  const code = await start(); await approve(code);
  const response = await poll(code); expect(response.status).toBe(200);
  return await response.json() as { access_token: string; token_type: string; expires_in: number };
}
beforeAll(async () => { fixture = await identityFixture({ ...identityEnv, oidc: null }); }, 30000);
beforeEach(async () => {
  const response = await fixture.request("/api/auth/sign-in/email", { email: identityEnv.adminEmail, password: identityEnv.adminPassword });
  expect(response.status).toBe(200); cookie = responseCookies(response);
});
afterAll(async () => { await fixture?.postgres.close(); });

it("requires an explicit claimed browser approval, honors polling, and consumes the grant once", async () => {
  const code = await start();
  expect((await fixture.request("/api/auth/device/approve", { userCode: code.user_code }, cookie)).status).toBe(403);
  const pending = await poll(code); expect(pending.status).toBe(400); expect(await pending.json()).toMatchObject({ error: "authorization_pending" });
  expect(await (await poll(code)).json()).toMatchObject({ error: "slow_down" });
  const claimed = await claim(code);
  expect(claimed.headers.get("cache-control")).toBe("no-store");
  expect(await claimed.json()).toMatchObject({ status: "pending", client_id: clientId });
  const [unapproved] = await fixture.db.select().from(schema.deviceCode).where(eq(schema.deviceCode.deviceCode, code.device_code));
  expect(unapproved?.status).toBe("pending");
  expect((await fixture.request("/api/auth/device/approve", { userCode: code.user_code }, cookie)).status).toBe(200);
  await fixture.db.update(schema.deviceCode).set({ lastPolledAt: null }).where(eq(schema.deviceCode.deviceCode, code.device_code));
  const result = await poll(code); expect(result.status).toBe(200);
  const issued = await result.json() as { access_token: string; token_type: string; expires_in: number };
  expect(issued.token_type).toBe("Bearer"); expect(issued.expires_in).toBeGreaterThan(29 * 86400);
  const [session] = await fixture.db.select().from(schema.session).where(eq(schema.session.token, issued.access_token));
  expect(session?.sessionKind).toBe("cli");
  expect(await (await poll(code)).json()).toMatchObject({ error: "invalid_grant" });
  const identity = await bearer(issued.access_token); expect(identity.status).toBe(200);
  expect(await identity.json()).toMatchObject({ principal: { kind: "user", email: identityEnv.adminEmail }, credential: { kind: "cliSession" }, session: { id: session!.id } });
});

it("keeps browser, CLI and API-key namespaces separate without cookie fallback", async () => {
  const cli = await loginCli();
  expect((await bearer(cli.access_token, "/api/ui/me", "GET", undefined, { cookie })).status).toBe(401);
  expect((await bearer(cli.access_token, "/api/auth/get-session")).status).toBe(401);
  expect((await bearer("invalid", "/api/v1/platform/session", "GET", undefined, { cookie })).status).toBe(401);
  const browser = await fixture.auth.api.getSession({ headers: new Headers({ cookie }) });
  expect(browser?.session.sessionKind).toBe("browser");
  expect((await bearer(browser!.session.token)).status).toBe(401);
  const minted = await bearer(cli.access_token, "/api/v1/platform/credentials", "POST", { name: "Created by CLI" });
  expect(minted.status).toBe(201);
  const key = await minted.json() as { secret: string };
  expect(await (await bearer(key.secret)).json()).toMatchObject({ credential: { kind: "personalKey" }, session: null });
  expect((await bearer(key.secret, "/api/v1/platform/credentials", "POST", { name: "Forbidden" })).status).toBe(403);
  expect((await bearer(key.secret, "/api/v1/platform/session", "DELETE")).status).toBe(403);
});

it("renews active CLI sessions with Better Auth and enforces the absolute lifetime", async () => {
  const cli = await loginCli();
  await fixture.db.update(schema.session).set({ createdAt: new Date(Date.now() - 2 * 86400000), updatedAt: new Date(Date.now() - 2 * 86400000), expiresAt: new Date(Date.now() + 3600000) }).where(eq(schema.session.token, cli.access_token));
  const renewed = await bearer(cli.access_token); expect(renewed.status).toBe(200);
  expect(renewed.headers.get("set-auth-token")).toBeTruthy();
  expect(renewed.headers.get("set-cookie")).toBeNull();
  expect((await bearer(renewed.headers.get("set-auth-token")!)).status).toBe(200);
  expect((await fixture.request("/api/ui/me", undefined, `${cookie.split("=")[0]}=${renewed.headers.get("set-auth-token")!}`)).status).toBe(401);
  const view = await renewed.json() as { session: { expiresAt: string } };
  expect(Date.parse(view.session.expiresAt) - Date.now()).toBeGreaterThan(29 * 86400000);
  await fixture.db.update(schema.session).set({ createdAt: new Date(Date.now() - 91 * 86400000) }).where(eq(schema.session.token, cli.access_token));
  expect((await bearer(cli.access_token)).status).toBe(401);
});

it("logout revokes only the current CLI session", async () => {
  const first = await loginCli(); const second = await loginCli();
  const logout = await bearer(first.access_token, "/api/v1/platform/session", "DELETE");
  expect(logout.status).toBe(200); expect(await logout.json()).toEqual({ revoked: true });
  expect((await bearer(first.access_token)).status).toBe(401);
  expect((await bearer(second.access_token)).status).toBe(200);
  expect((await fixture.request("/api/ui/me", undefined, cookie)).status).toBe(200);
});

it("refuses arbitrary clients, user prebinding, scopes and cross-origin approvals", async () => {
  for (const body of [{ client_id: "other" }, { client_id: clientId, user_id: "other" }, { client_id: clientId, scope: "admin" }, { client_id: clientId, approvalAccessVersion: 0 }]) {
    expect((await fixture.request("/api/auth/device/code", body)).status).toBe(400);
  }
  expect((await fixture.request("/api/auth/device/code", { client_id: "x".repeat(5000) })).status).toBe(400);
  const code = await start(); await claim(code);
  const response = await fixture.app.request(new Request(`${fixture.env.baseUrl}/api/auth/device/approve`, { method: "POST", headers: { cookie, origin: "https://attacker.example", "content-type": "application/json" }, body: JSON.stringify({ userCode: code.user_code }) }));
  expect(response.status).toBe(403);
  expect(await (await poll(code)).json()).toMatchObject({ error: "authorization_pending" });
});

it("rejects denied, expired and changed-admission grants", async () => {
  const denied = await start(); await claim(denied);
  expect((await fixture.request("/api/auth/device/deny", { userCode: denied.user_code }, cookie)).status).toBe(200);
  expect(await (await poll(denied)).json()).toMatchObject({ error: "access_denied" });
  const expired = await start();
  await fixture.db.update(schema.deviceCode).set({ expiresAt: new Date(Date.now() - 1) }).where(eq(schema.deviceCode.deviceCode, expired.device_code));
  expect(await (await poll(expired)).json()).toMatchObject({ error: "expired_token" });
  const changed = await start(); await approve(changed);
  const current = await fixture.auth.api.getSession({ headers: new Headers({ cookie }) });
  await fixture.db.update(schema.user).set({ accessVersion: (current!.user.accessVersion ?? 0) + 1 }).where(eq(schema.user.id, current!.user.id));
  expect(await (await poll(changed)).json()).toMatchObject({ error: "access_denied" });
});

it("never lets a different browser user approve a claimed device code", async () => {
  const code = await start(); await claim(code);
  const email = `other-${crypto.randomUUID()}@example.test`;
  expect((await fixture.request("/api/auth/admin/create-user", { email, name: "Another user", password: "another-user-password", role: "user" }, cookie)).status).toBe(200);
  const signedIn = await fixture.request("/api/auth/sign-in/email", { email, password: "another-user-password" });
  const otherCookie = responseCookies(signedIn);
  expect((await (await claim(code, otherCookie)).json())).not.toHaveProperty("client_id");
  expect((await fixture.request("/api/auth/device/approve", { userCode: code.user_code }, otherCookie)).status).toBe(403);
});
