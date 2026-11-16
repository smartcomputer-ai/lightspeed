import { expect, it, vi } from "vitest";
import { buildApp } from "./api.js";
import type { AppContext } from "./context.js";
import { identityEnv } from "../test/identity-fixture.js";

it("does not serve the organization plugin's endpoints; membership changes go through the universe routes", async () => {
  const handler = vi.fn(async () => new Response("handled"));
  const app = buildApp({ auth: { handler, api: { getSession: vi.fn() } }, env: {} } as unknown as AppContext);
  for (const path of ["/api/auth/organization/add-member", "/api/auth/organization/update-member-role", "/api/auth/organization/create"]) {
    expect((await app.request(path, { method: "POST" })).status).toBe(404);
  }
  expect(handler).not.toHaveBeenCalled();
  expect(await (await app.request("/api/auth/get-session")).text()).toBe("handled");
});

it.each([true, false, null])("serves public login settings without a session when automatic sign-in is %s", async (autoSignIn) => {
  const getSession = vi.fn();
  const env = { ...identityEnv, oidc: autoSignIn === null ? null : { ...identityEnv.oidc!, autoSignIn } };
  const app = buildApp({ auth: { api: { getSession } }, env } as unknown as AppContext);
  const response = await app.request("/api/login-config");
  expect(response.status).toBe(200);
  expect(await response.json()).toEqual({
    sso: autoSignIn !== null,
    providerId: autoSignIn === null ? null : expect.stringMatching(/^company-/),
    password: autoSignIn === null ? "local" : "break-glass",
    autoSignIn: autoSignIn ?? false,
  });
  expect(getSession).not.toHaveBeenCalled();
});

it("protects the shared API with browser authentication and typed HTTP problems", async () => {
  const app = buildApp({ auth: { api: { getSession: async () => null } }, env: {} } as unknown as AppContext);
  const response = await app.request("/api/v1/universes/acme/profiles");
  expect(response.status).toBe(401);
  expect(response.headers.get("content-type")).toBe("application/problem+json");
  expect(await response.json()).toMatchObject({ kind: "unauthenticated", status: 401, detail: "authentication required" });
});

it.each([
  { origin: "https://evil.example", cookie: "session=browser" },
  { cookie: "session=browser", "sec-fetch-site": "cross-site" },
  { cookie: "session=browser" },
])("rejects unsafe cookie mutations before reaching the runtime: %j", async headers => {
  const getSession = async () => ({ user: { id: "alice" }, session: { expiresAt: new Date(Date.now() + 60_000), createdAt: new Date() } });
  const app = buildApp({ auth: { api: { getSession } }, env: { baseUrl: "https://platform.example", trustedOrigins: [] } } as unknown as AppContext);
  const response = await app.request("/api/v1/universes/acme/sessions", { method: "POST", headers, body: "{}" });
  expect(response.status).toBe(403);
  expect(await response.json()).toMatchObject({ kind: "forbidden", status: 403 });
});
