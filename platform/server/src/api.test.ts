import { expect, it, vi } from "vitest";
import { buildApp } from "./api.js";
import type { AppContext } from "./context.js";

it("does not serve the organization plugin's endpoints; membership changes go through the universe routes", async () => {
  const handler = vi.fn(async () => new Response("handled"));
  const app = buildApp({ auth: { handler, api: { getSession: vi.fn() } }, env: {} } as unknown as AppContext);
  for (const path of ["/api/auth/organization/add-member", "/api/auth/organization/update-member-role", "/api/auth/organization/create"]) {
    expect((await app.request(path, { method: "POST" })).status).toBe(404);
  }
  expect(handler).not.toHaveBeenCalled();
  expect(await (await app.request("/api/auth/get-session")).text()).toBe("handled");
});
