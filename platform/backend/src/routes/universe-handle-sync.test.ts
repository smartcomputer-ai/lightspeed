import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { eq } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { organizationFixture } from "../../test/organization-fixture.js";
import { decodeHttpRequest, problemResponse } from "../../../test/http-fixture.js";

let fixture: Awaited<ReturnType<typeof organizationFixture>>;
let calls: Array<ReturnType<typeof decodeHttpRequest>>;
let runtimeHandle = "runtime-handle";
let rejectRename = false;
beforeEach(async () => {
  fixture = await organizationFixture(); calls = []; runtimeHandle = "runtime-handle"; rejectRename = false;
  vi.stubGlobal("fetch", vi.fn(async (url: string | Request | URL, init?: RequestInit) => {
    const request = decodeHttpRequest(url, init); calls.push(request);
    if (request.operation === "deployment/universes/list") return Response.json({ universes: [{ universeId: fixture.universe.lightspeedUniverseId, handle: runtimeHandle }] });
    if (rejectRename) return problemResponse({ type: "about:blank", title: "Conflict", detail: "handle already in use", kind: "conflict", status: 409 });
    runtimeHandle = String(request.params.handle);
    return Response.json({ universe: { universeId: fixture.universe.lightspeedUniverseId, handle: runtimeHandle } });
  }));
}, 30000);
afterEach(async () => { vi.unstubAllGlobals(); await fixture?.postgres.close(); });
const path = () => `/api/ui/universes/${fixture.universe.id}`;
it("serves cached lists and reads without contacting an unavailable runtime", async () => {
  vi.mocked(fetch).mockRejectedValue(new Error("offline"));
  expect(await (await fixture.request("/api/ui/universes")).json()).toMatchObject([{ handle: "work" }]);
  expect(await (await fixture.request(path())).json()).toMatchObject({ handle: "work" });
  expect(fetch).not.toHaveBeenCalled();
});
it("syncs only for Platform Admins and keeps the organization's slug independent", async () => {
  expect((await fixture.request("/api/ui/universes/sync-handles", "POST")).status).toBe(403);
  await fixture.asUser("owner", true);
  const response = await fixture.request("/api/ui/universes/sync-handles", "POST");
  expect(response.status).toBe(200);
  expect(await response.json()).toEqual({ updated: 1, skipped: 0 });
  expect((await fixture.db.select().from(schema.universes))[0]?.handle).toBe(runtimeHandle);
  expect((await fixture.db.select().from(schema.organization).where(eq(schema.organization.id, "organization-a")))[0]?.slug).toBe("a");
});
it("preserves the cache when runtime synchronization fails", async () => {
  await fixture.asUser("owner", true); vi.mocked(fetch).mockRejectedValue(new Error("offline"));
  expect((await fixture.request("/api/ui/universes/sync-handles", "POST")).status).toBe(502);
  expect((await fixture.db.select().from(schema.universes))[0]?.handle).toBe("work");
});
it("renames through runtime and updates only the universe handle", async () => {
  const response = await fixture.request(`${path()}/handle`, "PUT", { handle: "renamed" });
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject({ handle: "renamed" });
  expect(calls).toHaveLength(1);
  expect(calls[0]?.headers.get("x-lightspeed-actor")).toBe("owner");
  expect((await fixture.db.select().from(schema.organization).where(eq(schema.organization.id, "organization-a")))[0]?.slug).toBe("a");
});
it("keeps the old cache if runtime refuses a rename", async () => {
  rejectRename = true;
  expect((await fixture.request(`${path()}/handle`, "PUT", { handle: "taken" })).status).toBe(409);
  expect((await fixture.db.select().from(schema.universes))[0]?.handle).toBe("work");
});
it("refuses unauthorized renames and known universe handle collisions before contacting runtime", async () => {
  await fixture.asUser("viewer");
  expect((await fixture.request(`${path()}/handle`, "PUT", { handle: "new" })).status).toBe(403);
  await fixture.asUser("owner");
  await fixture.db.insert(schema.universes).values({ organizationId: "organization-a", handle: "taken", name: "Other", lightspeedUniverseId: crypto.randomUUID() });
  expect((await fixture.request(`${path()}/handle`, "PUT", { handle: "taken" })).status).toBe(409);
  expect(fetch).not.toHaveBeenCalled();
});
