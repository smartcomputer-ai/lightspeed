import { createHash } from "node:crypto";
import { Hono } from "hono";
import { beforeEach, expect, it, vi } from "vitest";
import { modulePermissions, universeActions, type UniverseRole } from "@lightspeed-ai/platform-shared";
import type { ModuleServices, UniverseModuleEnv } from "@lightspeed-ai/platform-backend/modules";
import { ARTIFACT_MAX_BYTES } from "../contract.js";
import { artifacts } from "../module.js";
import * as repository from "../db/index.js";
import artifactRoutes from "./index.js";

vi.mock("../db/index.js", () => ({
  listArtifacts: vi.fn(), getArtifact: vi.fn(), getArtifactContent: vi.fn(), publishArtifact: vi.fn(), retractArtifact: vi.fn(),
}));

const id = "3fa85f64-5717-4562-b3fc-2c963f66afa6";
const bytes = Buffer.from("report\n");
const blobRef = `sha256:${createHash("sha256").update(bytes).digest("hex")}`;
const publication = { workspaceId: "design", path: "reports/q1.txt", expectedRevision: 2, title: "Report" };

function mount(role: UniverseRole) {
  const app = new Hono<UniverseModuleEnv>();
  const scope = { universeId: "universe", organizationId: "organization", actorId: "person", actions: universeActions([role]) };
  const call = vi.fn();
  const openDownload = vi.fn();
  const services = { db: {} } as ModuleServices;
  app.use("*", async (c, next) => {
    c.set("access", scope);
    c.set("permissions", modulePermissions(artifacts, scope.actions));
    c.set("client", { call, openDownload });
    await next();
  });
  app.route("/", artifactRoutes(services));
  function source(size = bytes.length, response = new Response(bytes)) {
    const entry = { kind: "file", blob_ref: blobRef, size_bytes: size, media_type: "text/plain" };
    call.mockResolvedValueOnce({ workspace: { headSnapshotRef: "immutable-head", revision: 2 } })
      .mockResolvedValueOnce({ manifest: { schema_version: "lightspeed.vfs.snapshot.v1", root: {
        entries: { reports: { kind: "directory", entries: { "q1.txt": entry } } },
      } } });
    openDownload.mockResolvedValue(response);
  }
  function publish(input: unknown = publication) {
    return app.request("/", { method: "POST", body: JSON.stringify(input), headers: { "content-type": "application/json" } });
  }
  return { app, publish, services, source, call, openDownload, scope };
}

beforeEach(() => vi.resetAllMocks());

it.each([
  ["viewer", ["read"]],
  ["contributor", ["read", "publish"]],
  ["operator", ["read", "publish", "retract"]],
  ["owner", ["read", "publish", "retract"]],
] as const)("resolves the module's local permissions for a %s", (role, permissions) => {
  expect([...modulePermissions(artifacts, universeActions([role]))]).toEqual(permissions);
});

it("publishes only bytes from the selected revision, with trusted scope and digest", async () => {
  const { source, publish, openDownload, call, services, scope } = mount("contributor");
  source();
  vi.mocked(repository.publishArtifact).mockResolvedValue({
    ...publication, id, sizeBytes: bytes.length, mediaType: "text/plain", blobRef, createdAt: "2026-01-01T12:00:00Z", createdBy: scope.actorId,
  });
  const res = await publish();
  expect(res.status).toBe(201);
  expect(await res.json()).toHaveProperty("artifact.id", id);
  expect(call.mock.calls).toEqual([
    ["vfs/workspaces/read", { workspaceId: "design" }], ["vfs/snapshots/read", { snapshotRef: "immutable-head" }],
  ]);
  expect(openDownload).toHaveBeenCalledWith("blobs/read", { blobRef }, expect.objectContaining({ signal: expect.anything() }));
  expect(repository.publishArtifact).toHaveBeenCalledWith(services.db, scope, {
    workspaceId: "design", path: "reports/q1.txt", title: "Report", sizeBytes: bytes.length, mediaType: "text/plain", blobRef, content: bytes,
  });
});

it("rejects a stale selection before downloading the file", async () => {
  const { source, publish, call, openDownload } = mount("contributor");
  source();
  expect((await publish({ ...publication, expectedRevision: 1 })).status).toBe(409);
  expect(call).toHaveBeenCalledTimes(1);
  expect(openDownload).not.toHaveBeenCalled();
  expect(repository.publishArtifact).not.toHaveBeenCalled();
});

it("validates the path and rejects attempts to supply storage scope, publisher or digest", async () => {
  const { publish, call } = mount("contributor");
  for (const change of [{ path: "../plans" }, { blobRef }, { organizationId: "other" }, { createdBy: "other" }]) {
    expect((await publish({ ...publication, ...change })).status).toBe(400);
  }
  expect(call).not.toHaveBeenCalled();
});

it("checks the snapshot limit before download and aborts an oversized stream", async () => {
  const { source, publish, openDownload } = mount("contributor");
  source(ARTIFACT_MAX_BYTES + 1);
  expect((await publish()).status).toBe(413);
  expect(openDownload).not.toHaveBeenCalled();

  const cancel = vi.fn();
  source(1, new Response(new ReadableStream({ start(c) { c.enqueue(bytes); }, cancel })));
  expect((await publish()).status).toBe(502);
  expect(cancel).toHaveBeenCalledOnce();
  expect(repository.publishArtifact).not.toHaveBeenCalled();
});

it("rejects mismatched bytes of the same length", async () => {
  const { source, publish } = mount("contributor");
  source(bytes.length, new Response(Buffer.alloc(bytes.length)));
  expect((await publish()).status).toBe(502);
  expect(repository.publishArtifact).not.toHaveBeenCalled();
});

it("reads retained content through its scoped record without a runtime dependency", async () => {
  const { app, services, scope, call, openDownload } = mount("viewer");
  vi.mocked(repository.getArtifactContent).mockResolvedValue({ content: bytes, mediaType: "text/html" });
  const res = await app.request(`/${id}/content`);
  expect(res.status).toBe(200);
  expect(res.headers.get("cache-control")).toContain("no-store");
  expect(res.headers.get("content-type")).toContain("application/json");
  expect(await res.json()).toEqual({ bytesBase64: bytes.toString("base64"), bytes: bytes.length, mediaType: "text/html" });
  expect(repository.getArtifactContent).toHaveBeenCalledWith(services.db, scope, id);
  expect(call).not.toHaveBeenCalled();
  expect(openDownload).not.toHaveBeenCalled();
});

it("filters the shelf by workspace and returns the server's available actions", async () => {
  const { app, services, scope } = mount("contributor");
  vi.mocked(repository.listArtifacts).mockResolvedValue([]);
  expect(await (await app.request("/?workspaceId=design")).json()).toEqual({ artifacts: [], permissions: { publish: true, retract: false } });
  expect(repository.listArtifacts).toHaveBeenCalledWith(services.db, scope, "design");
});

it("returns 404 for malformed and unknown IDs", async () => {
  const { app } = mount("operator");
  expect((await app.request("/invalid/content")).status).toBe(404);
  expect(repository.getArtifactContent).not.toHaveBeenCalled();
  expect((await app.request("/" + id)).status).toBe(404);
  expect((await app.request("/" + id, { method: "DELETE" })).status).toBe(404);
});
