import { afterEach, beforeEach, expect, it } from "vitest";
import { schema } from "@lightspeed-ai/platform-db";
import { organizationFixture } from "../../test/organization-fixture.js";

const module = { id: "notebooks", name: "Notebooks", description: "Project notebooks", defaultEnabled: false };
const inventory = "/organizations/organization-a/modules";
let fixture: Awaited<ReturnType<typeof organizationFixture>>;
beforeEach(async () => { fixture = await organizationFixture([module]); }, 30000);
afterEach(async () => { await fixture?.postgres.close(); });

it("applies organization choices only to the selected distribution and organization", async () => {
  await fixture.asUser("organization-admin");
  const enabled = async () => (await fixture.request(inventory)).json();
  expect(await enabled()).toMatchObject({ modules: [{ id: "notebooks", enabled: false }] });
  for (let i = 0; i < 2; i++) {
    expect((await fixture.request(`${inventory}/notebooks`, "PUT", { enabled: true })).status).toBe(200);
  }
  expect(await enabled()).toMatchObject({ modules: [{ id: "notebooks", enabled: true }] });
  expect(await fixture.db.select().from(schema.organizationModules)).toHaveLength(1);
  expect((await fixture.request(`${inventory}/unavailable`, "PUT", { enabled: true })).status).toBe(404);
  await fixture.asUser("other");
  expect(await (await fixture.request("/organizations/organization-b/modules")).json())
    .toMatchObject({ modules: [{ id: "notebooks", enabled: false }] });
  expect((await fixture.request(inventory)).status).toBe(404);
});

it("lets members see available modules and requires organization policy authority to change them", async () => {
  await fixture.asUser("owner"); // A universe Owner is an ordinary member of organization-a.
  expect((await fixture.request(inventory)).status).toBe(200);
  const change = () => fixture.request(`${inventory}/notebooks`, "PUT", { enabled: true });
  expect((await change()).status).toBe(403);
  const admin = await fixture.asUser("organization-admin");
  fixture.setSubject({ ...admin, credential: { ...admin.credential, restrictions: { readOnly: true } } });
  expect((await change()).status).toBe(403);
  expect(await fixture.db.select().from(schema.organizationModules)).toHaveLength(0);
});
