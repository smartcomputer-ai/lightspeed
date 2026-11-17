import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { schema } from "@lightspeed-ai/platform-db";
import { seedDevelopment } from "./development-seed.js";
import { identityEnv, identityFixture } from "../test/identity-fixture.js";
import { decodeHttpRequest } from "../../test/http-fixture.js";

vi.mock("better-auth/crypto", async importOriginal => ({ ...await importOriginal<typeof import("better-auth/crypto")>(), hashPassword: vi.fn(async () => "hashed-password") }));
let fixture: Awaited<ReturnType<typeof identityFixture>>;
let handle = "test";
beforeEach(async () => {
  fixture = await identityFixture({ ...identityEnv, oidc: null, personalOrganizations: false, devSeed: true, lightspeedApiUrl: "https://runtime.example/api/v1", lightspeedApiToken: "seed-runtime-token-12345678901234567890" });
  handle = "test";
  vi.stubGlobal("fetch", vi.fn(async (url: string | Request | URL, init?: RequestInit) => {
    const request = decodeHttpRequest(url, init);
    expect(request.operation).toBe("deployment/universes/create");
    return Response.json({ created: true, universe: { universeId: request.params.universeId, handle } });
  }));
}, 30000);
afterEach(async () => { vi.unstubAllGlobals(); await fixture?.postgres.close(); });
it("does nothing unless development seeding is explicitly enabled", async () => {
  await seedDevelopment(fixture.db, { ...fixture.env, devSeed: false });
  expect(fetch).not.toHaveBeenCalled();
  expect(await fixture.db.select().from(schema.organization)).toEqual([]);
});
it("creates one shared development organization, Creator memberships and separate universe grants", async () => {
  await seedDevelopment(fixture.db, fixture.env);
  expect(await fixture.db.select().from(schema.organization)).toMatchObject([{ slug: "development", personalUserId: null, defaultProtected: false }]);
  const memberships = await fixture.db.select().from(schema.member);
  expect(memberships.map(member => member.role).sort()).toEqual(["admin", "creator", "creator", "creator"]);
  expect((await fixture.db.select().from(schema.universeGrants)).map(grant => grant.role).sort()).toEqual(["contributor", "operator", "owner", "viewer"]);
  expect(await fixture.db.select().from(schema.universes)).toMatchObject([{ handle: "test", features: { bots: true, channels: true } }]);
});
it("reruns without duplicating organizations, universe grants or credential logins", async () => {
  await seedDevelopment(fixture.db, fixture.env); await seedDevelopment(fixture.db, fixture.env);
  expect(await fixture.db.select().from(schema.organization)).toHaveLength(1);
  expect(await fixture.db.select().from(schema.universes)).toHaveLength(1);
  expect(await fixture.db.select().from(schema.universeGrants)).toHaveLength(4);
  expect(await fixture.db.select().from(schema.account)).toHaveLength(4);
});
it("preserves runtime renames without changing the organization slug", async () => {
  await seedDevelopment(fixture.db, fixture.env); handle = "renamed-test";
  await seedDevelopment(fixture.db, fixture.env);
  expect((await fixture.db.select().from(schema.universes))[0]?.handle).toBe("renamed-test");
  expect((await fixture.db.select().from(schema.organization))[0]?.slug).toBe("development");
});
