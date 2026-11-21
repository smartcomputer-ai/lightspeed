import { type PlatformModule, withModuleRoutes } from "@lightspeed-ai/platform-shared";
import { Hono } from "hono";
import { expect, it } from "vitest";
import { DEMO_USER } from "./fixtures";
import { createDemoRouter } from "./router";
import { DemoStore } from "./store";

const reports: PlatformModule = {
  id: "reports", name: "Reports", description: "", defaultEnabled: true,
  permissions: { export: "configureResource" },
  operations: [{ id: "export", method: "GET", path: "/summary", permission: "export" }],
};

it("serves browser and public contributions independently under one organization setting", async () => {
  const store = new DemoStore({ ...DEMO_USER, role: "user" });
  const { universe } = store.addUniverse({ handle: "work", name: "Work" });
  const modules = withModuleRoutes([reports], { reports: {
    browserRoutes: () => new Hono().get("/summary", c => c.text("preview")),
    publicOperations: () => new Hono().get("/summary", c => c.text("export")),
  } });
  const app = createDemoRouter(store, modules);
  const path = `/universes/${universe.id}/modules/reports/summary`;
  const browser = () => app.request(`/api/ui${path}`);
  const api = () => app.request(`/api/v1${path}`);

  expect(await (await browser()).text()).toBe("preview");
  expect(await (await api()).text()).toBe("export");

  universe.organizationRole = "member";
  universe.role = "viewer";
  expect((await browser()).status).toBe(200);
  expect((await api()).status).toBe(403);

  const disabled = await app.request(`/api/v1/platform/organizations/${universe.organizationId}/modules/reports`, {
    method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ enabled: false }),
  });
  expect(disabled.status).toBe(200);
  expect((await browser()).status).toBe(404);
  expect((await api()).status).toBe(404);
});

it("accepts production operation declarations when the module omits its public demo adapter", async () => {
  const store = new DemoStore({ ...DEMO_USER });
  const { universe } = store.addUniverse({ handle: "work", name: "Work" });
  const app = createDemoRouter(store, [{ ...reports, browserRoutes: () => new Hono().get("/summary", c => c.text("preview")) }]);
  const path = `/universes/${universe.id}/modules/reports/summary`;
  expect(await (await app.request(`/api/ui${path}`)).text()).toBe("preview");
  expect((await app.request(`/api/v1${path}`)).status).toBe(404);
});

it("rejects a public demo factory without declared operations even if the factory supplies no handlers", () => {
  const store = new DemoStore({ ...DEMO_USER });
  expect(() => createDemoRouter(store, [{ ...reports, operations: [], publicOperations: () => new Hono() }]))
    .toThrow("without declared operations");
});
