import { createHash } from "node:crypto";
import { eq } from "drizzle-orm";
import { Hono } from "hono";
import { afterAll, afterEach, beforeAll, expect, it, vi } from "vitest";
import { buildApp, migrateModuleDatabases } from "@lightspeed-ai/platform-backend/server";
import type { ApiModule, UniverseModuleEnv } from "@lightspeed-ai/platform-backend/modules";
import { schema } from "@lightspeed-ai/platform-db";
import { HTTP_CONTRACT_REVISION } from "@lightspeed-ai/sdk/server";
import { buildStandardApi, standardApiModules } from "../../api.js";
import { organizationFixture } from "../../../../platform/backend/test/organization-fixture.js";
import { responseCookies } from "../../../../platform/backend/test/identity-fixture.js";
import { decodeHttpRequest } from "../../../../platform/test/http-fixture.js";
import type { Artifact } from "../../../../modules/artifacts/contract.js";

const root = "/api/v1/universes/work/modules/artifacts";
const bytes = Buffer.from("Published from the reviewed draft.\n");
const blobRef = "sha256:" + createHash("sha256").update(bytes).digest("hex");
const publish = { workspaceId: "notes", path: "review.md", expectedRevision: 7, title: "Release review" };
let fixture: Awaited<ReturnType<typeof organizationFixture>>;
let app: ReturnType<typeof buildStandardApi>;
let cookie: string;

beforeAll(async () => {
  fixture = await organizationFixture();
  await migrateModuleDatabases(fixture.db, standardApiModules);
  app = buildStandardApi(fixture.context);
  const login = await app.request("/api/auth/sign-in/email", {
    method: "POST", headers: { "content-type": "application/json", origin: fixture.env.baseUrl },
    body: JSON.stringify({ email: fixture.env.adminEmail, password: fixture.env.adminPassword }),
  });
  expect(login.status).toBe(200);
  cookie = responseCookies(login);
  await fixture.db.insert(schema.universes).values(["organization-a", "organization-b"].map((org, i) => ({
    organizationId: org, name: `Other ${i}`, handle: `other-${i}`, lightspeedUniverseId: crypto.randomUUID(),
    createdBy: "owner", runtimeConfiguration: { protected: false },
  })));
}, 30_000);
afterAll(async () => { await fixture?.postgres.close(); });
afterEach(() => vi.unstubAllGlobals());

function request(path: string, method = "GET", body?: unknown, bearer?: string) {
  return app.request(path, {
    method, headers: {
      ...(bearer ? { authorization: `Bearer ${bearer}` } : { cookie }),
      "content-type": "application/json", origin: fixture.env.baseUrl,
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
}

it("refuses to start with a mismatch between public operations and implemented endpoints", () => {
  const api: ApiModule = {
    id: "sample", name: "Sample", description: "", defaultEnabled: true,
    permissions: { read: "read" }, operations: [{ id: "list", method: "GET", path: "/", permission: "read" }],
  };
  expect(() => buildApp(fixture.context, [api])).toThrow(/without an API implementation/);
  expect(() => buildApp(fixture.context, [{ ...api, routes: () => new Hono<UniverseModuleEnv>() }])).toThrow(/missing the handler/);
  const router = new Hono<UniverseModuleEnv>().get("/", c => c.json([])).post("/forgotten", c => c.text("extra"));
  expect(() => buildApp(fixture.context, [{ ...api, routes: () => router }])).toThrow(/undeclared route/);
});

it("copies a permitted source once and isolates the publication across restarts, installations and tenants", async () => {
  const upstream = vi.fn(async (input: Request | URL | string, init?: RequestInit) => {
    const call = decodeHttpRequest(input, init);
    expect(call.universe).toBe(fixture.universe.lightspeedUniverseId);
    switch (call.method) {
      case "initialize": return Response.json({ contractRevision: HTTP_CONTRACT_REVISION });
      case "vfs/workspaces/read": return Response.json({ workspace: { revision: 7, headSnapshotRef: blobRef } });
      case "vfs/snapshots/read": return Response.json({ manifest: {
        schema_version: "lightspeed.vfs.snapshot.v1", root: { entries: {
          "review.md": { kind: "file", blob_ref: blobRef, size_bytes: bytes.length, media_type: "text/markdown", executable: false },
        } }, totals: { files: 1, bytes: bytes.length },
      } });
      case "blobs/read": return new Response(bytes, { headers: { "content-type": "application/octet-stream" } });
      default: throw new Error(`Unexpected upstream operation ${call.method}`);
    }
  });
  vi.stubGlobal("fetch", upstream);
  const created = await request(root, "POST", publish);
  expect(created.status).toBe(201);
  const { artifact } = await created.json() as { artifact: Artifact };
  expect(artifact).toMatchObject({ title: publish.title, path: publish.path, workspaceId: publish.workspaceId, blobRef });
  expect(artifact.createdBy).toBeTruthy();

  app = buildStandardApi(fixture.context);
  upstream.mockClear();
  const detail = `${root}/${artifact.id}`;
  expect(await (await request(root)).json()).toMatchObject({ artifacts: [artifact], permissions: { publish: true, retract: true } });
  for (const handle of ["other-0", "other-1"]) {
    const other = root.replace("/work/", `/${handle}/`);
    expect(await (await request(other)).json()).toMatchObject({ artifacts: [] });
    expect((await request(`${other}/${artifact.id}/content`)).status).toBe(404);
    expect((await request(`${other}/${artifact.id}`, "DELETE")).status).toBe(404);
  }
  const setting = "/api/v1/platform/organizations/organization-a/modules/artifacts";
  expect((await request(setting, "PUT", { enabled: false })).status).toBe(200);
  expect((await request(detail + "/content")).status).toBe(404);
  expect((await request(root, "POST", publish)).status).toBe(404);
  expect((await request(setting, "PUT", { enabled: true })).status).toBe(200);
  expect(await (await request(detail + "/content")).json()).toEqual({ bytesBase64: bytes.toString("base64"), bytes: bytes.length, mediaType: "text/markdown" });
  expect((await request(detail, "DELETE")).status).toBe(204);
  expect((await request(detail)).status).toBe(404);
  expect(upstream).not.toHaveBeenCalled();
});

it("intersects declared module permissions with credential restrictions before reading the source", async () => {
  const key = async (restrictions: object) => {
    const created = await request("/api/v1/platform/credentials", "POST", { name: "Artifacts reader", restrictions });
    expect(created.status).toBe(201);
    return ((await created.json()) as { secret: string }).secret;
  };
  const token = await key({ operations: ["artifacts/list"], universeIds: [fixture.universe.lightspeedUniverseId] });
  expect((await request(root, "GET", undefined, token)).status).toBe(200);
  expect(await (await request(root, "GET", undefined, token)).json()).toMatchObject({ permissions: { publish: false, retract: false } });
  const item = `${root}/${crypto.randomUUID()}`;
  expect((await request(item, "GET", undefined, token)).status).toBe(403);
  expect((await request(item + "/content", "GET", undefined, token)).status).toBe(403);
  expect((await request(root.replace("/work/", "/other-0/"), "GET", undefined, token)).status).toBe(404);
  expect((await request(root, "POST", publish, token)).status).toBe(403);
  const readOnly = await key({ readOnly: true });
  expect((await request(root, "POST", publish, readOnly)).status).toBe(403);
  expect((await request(item, "DELETE", undefined, readOnly)).status).toBe(403);
  expect((await request(root, "PATCH", {}, readOnly)).status).toBe(404);
  expect((await request(root.replace("/api/v1/", "/api/ui/"))).status).toBe(404);
  expect((await request(root.replace("artifacts", "bots"), "GET", undefined, readOnly)).status).toBe(404);
  expect((await app.request(root)).status).toBe(401);

  const [user] = await fixture.db.select().from(schema.user).where(eq(schema.user.email, fixture.env.adminEmail!));
  await fixture.db.update(schema.user).set({ role: "user" }).where(eq(schema.user.id, user!.id));
  await fixture.db.insert(schema.member).values({ id: "artifact-member", organizationId: "organization-a", userId: user!.id, role: "member", createdAt: new Date() });
  await fixture.db.insert(schema.universeGrants).values({ universeId: fixture.universe.id, userId: user!.id, role: "viewer" });
  try {
    expect(await (await request(root)).json()).toMatchObject({ permissions: { publish: false, retract: false } });
    expect((await request(root, "POST", publish)).status).toBe(403);
    await fixture.db.update(schema.universeGrants).set({ role: "contributor" }).where(eq(schema.universeGrants.userId, user!.id));
    expect(await (await request(root)).json()).toMatchObject({ permissions: { publish: true, retract: false } });
    expect((await request(item, "DELETE")).status).toBe(403);
  } finally {
    await fixture.db.update(schema.user).set({ role: "admin" }).where(eq(schema.user.id, user!.id));
  }
});

it("admits a service user only in its granted universe and stops access when the account is disabled", async () => {
  const [service] = await fixture.db.insert(schema.serviceUsers).values({
    organizationId: "organization-a", name: "Publication indexer", role: "member",
  }).returning();
  await fixture.db.insert(schema.universeGrants).values({
    universeId: fixture.universe.id, serviceUserId: service!.id, role: "viewer",
  });
  const issued = await request(`/api/v1/platform/organizations/organization-a/service-users/${service!.id}/credentials`, "POST", {
    name: "Read publication metadata", restrictions: { operations: ["artifacts/list"] },
  });
  expect(issued.status).toBe(201);
  const { secret } = await issued.json() as { secret: string };
  expect((await request(root, "GET", undefined, secret)).status).toBe(200);
  expect((await request(root.replace("/work/", "/other-1/"), "GET", undefined, secret)).status).toBe(404);
  expect((await request(root, "POST", publish, secret)).status).toBe(403);
  await fixture.db.update(schema.serviceUsers).set({ enabled: false }).where(eq(schema.serviceUsers.id, service!.id));
  expect((await request(root, "GET", undefined, secret)).status).toBe(401);
});
