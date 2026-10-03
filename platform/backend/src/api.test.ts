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
