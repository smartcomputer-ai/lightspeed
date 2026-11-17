import { afterEach, beforeEach, expect, it } from "vitest";
import { eq } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { organizationFixture } from "../../test/organization-fixture.js";
import { identityEnv, identityFixture } from "../../test/identity-fixture.js";
import { ensurePersonalOrganization } from "../personal-organizations.js";
import { platformSettings } from "../platform-settings.js";

let fixture: Awaited<ReturnType<typeof organizationFixture>>;
beforeEach(async () => { fixture = await organizationFixture(); }, 30000);
afterEach(async () => { await fixture?.postgres.close(); });

type Setting = { key: string; value: unknown; source: string; defaultValue: unknown; environmentVariable: string };
const settings = async () => new Map(((await (await fixture.request("/settings")).json()) as { settings: Setting[] }).settings.map(setting => [setting.key, setting]));
const organization = async (id: string) => ((await (await fixture.request(`/organizations/${id}`)).json()) as { organization: { universeQuota: number; universeQuotaSource: string } }).organization;

it("takes each setting from the environment first, then a value set here, then the default", async () => {
  await fixture.asUser("organization-admin", true);
  const listed = await settings();
  // The fixture's environment turns personal organizations off.
  expect(listed.get("personalOrganizations")).toMatchObject({ value: false, source: "environment", defaultValue: true, environmentVariable: "LIGHTSPEED_PLATFORM_PERSONAL_ORGANIZATIONS" });
  expect(listed.get("sharedUniverseQuota")).toMatchObject({ value: 100, source: "default" });
  const put = await fixture.request("/settings/sharedUniverseQuota", "PUT", { value: 7 });
  expect(put.status).toBe(200);
  expect(await put.json()).toMatchObject({ setting: { key: "sharedUniverseQuota", value: 7, source: "platform" } });
  expect((await fixture.request("/settings/sharedUniverseQuota", "PUT", { value: null })).status).toBe(200);
  expect((await settings()).get("sharedUniverseQuota")).toMatchObject({ value: 100, source: "default" });
});

it("refuses to change a setting the environment holds, a value of the wrong kind, and anyone but a Platform Admin", async () => {
  await fixture.asUser("organization-admin", true);
  expect((await fixture.request("/settings/personalOrganizations", "PUT", { value: true })).status).toBe(409);
  expect((await fixture.request("/settings/sharedUniverseQuota", "PUT", { value: true })).status).toBe(400);
  expect((await fixture.request("/settings/sharedUniverseQuota", "PUT", { value: -1 })).status).toBe(400);
  await fixture.asUser("organization-admin");
  expect((await fixture.request("/settings")).status).toBe(403);
  expect((await fixture.request("/settings/sharedUniverseQuota", "PUT", { value: 7 })).status).toBe(403);
  expect(await fixture.db.select().from(schema.platformSettings)).toEqual([]);
});

it("lets an organization follow the deployment default until it gets its own limit, and return to it", async () => {
  await fixture.asUser("organization-admin", true);
  expect(await organization("organization-b")).toMatchObject({ universeQuota: 100, universeQuotaSource: "deployment" });
  await fixture.request("/settings/sharedUniverseQuota", "PUT", { value: 3 });
  expect(await organization("organization-b")).toMatchObject({ universeQuota: 3, universeQuotaSource: "deployment" });
  expect(await organization("organization-a")).toMatchObject({ universeQuota: 2, universeQuotaSource: "organization" });
  expect((await fixture.request("/organizations/organization-a", "PATCH", { universeQuota: null })).status).toBe(200);
  expect(await organization("organization-a")).toMatchObject({ universeQuota: 3, universeQuotaSource: "deployment" });
});

it("creates personal organizations only while the setting allows it", async () => {
  const personal = await identityFixture({ ...identityEnv, oidc: null });
  try {
    await personal.db.insert(schema.user).values({ id: "dana", name: "Dana", email: "dana@example.test", emailVerified: true });
    await personal.db.insert(schema.platformSettings).values({ key: "personalOrganizations", value: false });
    expect((await platformSettings(personal.db, identityEnv)).personalOrganizations).toBe(false);
    await ensurePersonalOrganization(personal.db, identityEnv, "dana");
    expect(await personal.db.select().from(schema.organization).where(eq(schema.organization.personalUserId, "dana"))).toEqual([]);
    await personal.db.delete(schema.platformSettings);
    await ensurePersonalOrganization(personal.db, identityEnv, "dana");
    const [created] = await personal.db.select().from(schema.organization).where(eq(schema.organization.personalUserId, "dana"));
    expect(created?.universeQuota).toBeNull();
  } finally { await personal.postgres.close(); }
});
