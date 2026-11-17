import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { eq } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { organizationFixture } from "../../test/organization-fixture.js";
import { decodeHttpRequest, problemResponse } from "../../../test/http-fixture.js";

let fixture: Awaited<ReturnType<typeof organizationFixture>>;
let inventory: Map<string, Record<string, unknown>>;
let failConfiguration = false;
let requests: Array<ReturnType<typeof decodeHttpRequest>>;
const newId = "22222222-2222-4222-8222-222222222222";
beforeEach(async () => {
  fixture = await organizationFixture(); failConfiguration = false; requests = [];
  inventory = new Map([[fixture.universe.lightspeedUniverseId, runtimeView(fixture.universe.lightspeedUniverseId, "work")]]);
  vi.stubGlobal("fetch", vi.fn(async (url: string | Request | URL, init?: RequestInit) => {
    const request = decodeHttpRequest(url, init); requests.push(request);
    if (request.operation === "initialize") return Response.json({ runtime: "runtime", contractRevision: "1", caller: { single: false }, serverInfo: { name: "Runtime", version: "1" } });
    const id = String(request.params.universeId ?? request.params.universe);
    if (request.operation === "deployment/universes/create") {
      const prior = inventory.get(id);
      const universe = prior ?? { ...runtimeView(id, String(request.params.handle)), metadata: request.params.metadata ?? {}, protected: request.params.protected ?? false, expiresAtMs: request.params.expiresAtMs ?? null };
      inventory.set(id, universe); return Response.json({ created: !prior, universe });
    }
    if (request.operation === "deployment/universes/list") return Response.json({ universes: [...inventory.values()] });
    const current = inventory.get(id);
    if (!current) return problemResponse({ type: "about:blank", title: "Not found", detail: "universe not found", status: 404, kind: "notFound" });
    if (request.operation === "deployment/universes/configuration/put") {
      if (failConfiguration) throw new Error("interrupted");
      const { universe: _, ...changes } = request.params; Object.assign(current, changes);
    }
    if (request.operation === "deployment/universes/expiry/put") current.expiresAtMs = request.params.expiresAtMs;
    return Response.json({ universe: current });
  }));
}, 30000);
afterEach(async () => { vi.unstubAllGlobals(); await fixture?.postgres.close(); });
function runtimeView(id: string, handle: string) { return { universeId: id, handle, metadata: {}, protected: false, expiresAtMs: null, createdAtMs: 1, sessions: 0, profiles: 0, workspaces: 0, blobBytes: 0 }; }

it("creates into an explicit organization, reserves its quota and grants the current Creator initial ownership", async () => {
  await fixture.asUser("creator");
  const response = await fixture.request("/organizations/organization-a/universes", "POST", { universeId: newId, name: "New" });
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject({ universe: { organizationId: "organization-a", role: "owner", createdBy: "creator", lightspeedUniverseId: newId } });
  const [created] = await fixture.db.select().from(schema.universes).where(eq(schema.universes.lightspeedUniverseId, newId));
  expect(await fixture.db.select().from(schema.universeGrants).where(eq(schema.universeGrants.universeId, created!.id))).toMatchObject([{ userId: "creator", role: "owner" }]);
  expect((await fixture.request("/organizations/organization-a/universes", "POST", { name: "Over quota" })).status).toBe(409);
});
it("does not let an Owner who is only an organization Member create another universe", async () => {
  expect((await fixture.request("/organizations/organization-a/universes", "POST", { universeId: newId, name: "No" })).status).toBe(403);
  expect(requests).toHaveLength(0);
});
it("uses atomic runtime lifecycle settings and keeps a durable reservation on interruption", async () => {
  await fixture.asUser("creator");
  await fixture.db.update(schema.organization).set({ defaultProtected: true }).where(eq(schema.organization.id, "organization-a"));
  failConfiguration = true;
  expect((await fixture.request("/organizations/organization-a/universes", "POST", { universeId: newId, name: "Protected", metadata: { team: "research" } })).status).toBe(500);
  expect(inventory.get(newId)?.protected).toBe(true);
  expect(inventory.get(newId)?.metadata).toEqual({ team: "research" });
  expect((await fixture.db.select().from(schema.universes).where(eq(schema.universes.lightspeedUniverseId, newId)))[0]?.status).toBe("provisioning");
  failConfiguration = false;
  expect((await fixture.request("/organizations/organization-a/universes", "POST", { universeId: newId, name: "Protected" })).status).toBe(200);
  expect(await fixture.db.select().from(schema.universes)).toHaveLength(2);
  expect(inventory.get(newId)?.metadata).toEqual({ team: "research" });
});
it("revalidates mandatory protection when retrying a reserved creation", async () => {
  await fixture.asUser("creator"); failConfiguration = true;
  await fixture.request("/organizations/organization-a/universes", "POST", { universeId: newId, name: "Pending" });
  await fixture.db.update(schema.organization).set({ requireProtected: true }).where(eq(schema.organization.id, "organization-a"));
  failConfiguration = false;
  expect((await fixture.request("/organizations/organization-a/universes", "POST", { universeId: newId, name: "Pending" })).status).toBe(200);
  expect(inventory.get(newId)?.protected).toBe(true);
});
it("requires an explicit organization-member Owner for administrative adoption", async () => {
  inventory.set(newId, runtimeView(newId, "orphan"));
  await fixture.asUser("owner", true);
  expect((await fixture.request("/organizations/organization-a/universes/adopt", "POST", { lightspeedUniverseId: newId, name: "Adopted" })).status).toBe(400);
  const response = await fixture.request("/organizations/organization-a/universes/adopt", "POST", { lightspeedUniverseId: newId, name: "Adopted", initialOwner: { kind: "user", id: "creator" } });
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject({ universe: { organizationId: "organization-a", handle: "orphan", lightspeedUniverseId: newId } });
});
it("prevents Owners from clearing protection through both native and runtime-shaped configuration", async () => {
  expect((await fixture.request(`/universes/${fixture.universe.lightspeedUniverseId}`, "PATCH", { protected: false })).status).toBe(403);
  expect((await fixture.request(`/api/v1/universes/${fixture.universe.lightspeedUniverseId}/configuration`, "PUT", { protected: false })).status).toBe(403);
  expect(requests.some(request => request.operation === "deployment/universes/configuration/put")).toBe(false);
});
it("requires existing universes to be protected without expiry before making organization protection mandatory", async () => {
  await fixture.asUser("organization-admin");
  expect((await fixture.request("/organizations/organization-a", "PATCH", { requireProtected: true })).status).toBe(409);
  inventory.get(fixture.universe.lightspeedUniverseId)!.protected = true;
  expect((await fixture.request("/organizations/organization-a", "PATCH", { requireProtected: true })).status).toBe(200);
  expect((await fixture.request(`/universes/${fixture.universe.lightspeedUniverseId}`, "PATCH", { protected: false })).status).toBe(403);
});

it("cancels an absent provisioning reservation even when its organization requires protection", async () => {
  await fixture.asUser("creator"); failConfiguration = true;
  await fixture.request("/organizations/organization-a/universes", "POST", { universeId: newId, name: "Cancelled" });
  inventory.delete(newId);
  await fixture.db.update(schema.organization).set({ requireProtected: true }).where(eq(schema.organization.id, "organization-a"));
  const response = await fixture.request(`/api/v1/universes/${newId}`, "DELETE");
  expect(response.status).toBe(200);
  expect(await response.json()).toEqual({ universeId: newId, state: "deleted" });
  expect(await fixture.db.select().from(schema.universes).where(eq(schema.universes.lightspeedUniverseId, newId))).toHaveLength(0);
  expect(await fixture.db.select().from(schema.organization).where(eq(schema.organization.id, "organization-a"))).toHaveLength(1);
  expect(requests.some(request => request.operation === "deployment/universes/delete")).toBe(false);
});
it("keeps real runtime protection intact when a provisioning reservation exists", async () => {
  await fixture.asUser("creator"); failConfiguration = true;
  await fixture.request("/organizations/organization-a/universes", "POST", { universeId: newId, name: "Protected pending" });
  await fixture.db.update(schema.organization).set({ requireProtected: true }).where(eq(schema.organization.id, "organization-a"));
  const response = await fixture.request(`/api/v1/universes/${newId}`, "DELETE");
  expect(response.status).toBe(409);
  expect(await fixture.db.select().from(schema.universes).where(eq(schema.universes.lightspeedUniverseId, newId))).toHaveLength(1);
  expect(requests.some(request => request.operation === "deployment/universes/delete")).toBe(false);
});
it("repairs a missing runtime with mandatory protection applied atomically", async () => {
  await fixture.asUser("owner", true);
  await fixture.db.update(schema.organization).set({ requireProtected: true }).where(eq(schema.organization.id, "organization-a"));
  inventory.delete(fixture.universe.lightspeedUniverseId);
  const response = await fixture.request(`/api/ui/universes/${fixture.universe.id}/engine`, "POST");
  expect(response.status).toBe(200);
  expect(requests.find(request => request.operation === "deployment/universes/create")?.params).toMatchObject({ protected: true, expiresAtMs: null });
  expect(inventory.get(fixture.universe.lightspeedUniverseId)?.protected).toBe(true);
});


it("archives and restores the Platform presentation without deleting runtime data or grants", async () => {
  const id = fixture.universe.lightspeedUniverseId;
  inventory.get(id)!.protected = true;
  const grants = await fixture.db.select().from(schema.universeGrants);
  for (const status of ["archived", "active"]) {
    const response = await fixture.request(`/universes/${id}`, "PATCH", { status });
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({ universe: { status } });
    expect(inventory.get(id)?.protected).toBe(true);
    expect(await fixture.db.select().from(schema.universeGrants)).toEqual(grants);
  }
  expect(requests.some(request => request.operation === "deployment/universes/delete")).toBe(false);
  // A Creator sees the universe through the organization's base role, without administering it.
  await fixture.asUser("creator");
  expect((await fixture.request(`/universes/${id}`, "PATCH", { status: "archived" })).status).toBe(403);
});
