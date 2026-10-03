import { Hono } from "hono";
import { unzipSync } from "fflate";
import {
  MAX_WORKSPACE_UPLOAD_BYTES,
  workspaceUploadSchema,
} from "@lightspeed/platform-shared";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ApiVariables, AppContext } from "../context.js";
import { emptyManifest, type VfsManifest } from "../vfs.js";
import { gatewayRoutes } from "./gateway.js";

const identity = vi.hoisted(() => ({ role: "contributor" }));
vi.mock("./universes.js", () => ({
  universeForSession: vi.fn(async () => ({
    universe: {
      lightspeedUniverseId: "universe",
      gatewayUrl: "https://engine.example/rpc",
    },
    slug: "test",
    role: identity.role,
    member: { userId: "member", role: identity.role },
  })),
}));

let manifest: VfsManifest;
let committedManifest: VfsManifest | null;
let revision: number;
let calls: string[];
let blobs: Map<string, string>;
let failBlob: boolean;
let race: boolean;
let workspaceDeleted: boolean;
let displayName: string | undefined;
it("deletes a folder recursively in one revision and leaves siblings intact", async () => {
  await upload({}, [
    ...entries,
    { kind: "file", path: "keep.txt", contentBase64: "AA==" },
  ]);
  const response = await app().request(
    "/u/workspaces/docs/entries?path=reports&expectedRevision=1",
    { method: "DELETE" },
  );
  expect(response.status).toBe(200);
  expect(revision).toBe(2);
  expect(Object.keys(manifest.root.entries)).toEqual(["keep.txt"]);
  expect(manifest.totals).toEqual({ files: 1, bytes: 1 });
});

it("protects folder deletion with permissions and revision checks", async () => {
  await upload();
  calls = [];
  identity.role = "viewer";
  expect(
    (
      await app().request(
        "/u/workspaces/docs/entries?path=reports&expectedRevision=1",
        { method: "DELETE" },
      )
    ).status,
  ).toBe(403);
  expect(calls).toEqual([]);
  identity.role = "contributor";
  expect(
    (
      await app().request(
        "/u/workspaces/docs/entries?path=reports&expectedRevision=0",
        { method: "DELETE" },
      )
    ).status,
  ).toBe(409);
  expect(calls).not.toContain("vfs/snapshots/commit");
  for (const query of [
    "path=reports",
    "path=..&expectedRevision=1",
    "path=&expectedRevision=1",
  ]) {
    expect(
      (
        await app().request(`/u/workspaces/docs/entries?${query}`, {
          method: "DELETE",
        })
      ).status,
    ).toBe(400);
  }
  expect(
    (
      await app().request(
        "/u/workspaces/docs/entries?path=missing&expectedRevision=1",
        { method: "DELETE" },
      )
    ).status,
  ).toBe(404);
  expect(revision).toBe(1);
});
it("validates the full upload size without recursive regex overflow", () => {
  const contentBase64 = Buffer.alloc(MAX_WORKSPACE_UPLOAD_BYTES).toString(
    "base64",
  );
  const entry = { kind: "file", path: "large.bin", contentBase64 };
  expect(
    workspaceUploadSchema.safeParse({ expectedRevision: 0, entries: [entry] })
      .success,
  ).toBe(true);
  expect(
    workspaceUploadSchema.safeParse({
      expectedRevision: 0,
      entries: [entry, { ...entry, path: "extra", contentBase64: "AA==" }],
    }).success,
  ).toBe(false);
  for (const contentBase64 of ["!AAA", "A===", "AAA", "AA=A"]) {
    expect(
      workspaceUploadSchema.safeParse({
        expectedRevision: 0,
        entries: [{ ...entry, contentBase64 }],
      }).success,
    ).toBe(false);
  }
});
beforeEach(() => {
  identity.role = "contributor";
  manifest = emptyManifest();
  committedManifest = null;
  revision = 0;
  calls = [];
  blobs = new Map();
  failBlob = false;
  race = false;
  workspaceDeleted = false;
  displayName = undefined;
  vi.stubGlobal(
    "fetch",
    vi.fn(async (_url: unknown, init: RequestInit) => {
      const rpc = JSON.parse(String(init.body));
      calls.push(rpc.method);
      expect(new Headers(init.headers).get("x-lightspeed-universe")).toBe(
        "universe",
      );
      let result: unknown;
      switch (rpc.method) {
        case "vfs/workspaces/delete":
          expect(rpc.params).toEqual({ workspaceId: "docs" });
          if (workspaceDeleted)
            return Response.json({
              id: rpc.id,
              error: {
                code: -32004,
                message: "workspace not found",
                data: { kind: "not_found" },
              },
            });
          workspaceDeleted = true;
          result = {
            workspace: {
              workspaceId: "docs",
              revision,
              headSnapshotRef: "snapshot",
            },
          };
          break;
        case "vfs/workspaces/read":
          result = {
            workspace: {
              workspaceId: "docs",
              revision,
              headSnapshotRef: "snapshot",
              displayName,
            },
          };
          break;
        case "vfs/snapshots/read":
          expect(rpc.params.snapshotRef).toBe("snapshot");
          result = { manifest: structuredClone(manifest) };
          break;
        case "blobs/put": {
          if (failBlob)
            return Response.json({
              id: rpc.id,
              error: { code: -32603, message: "storage failed" },
            });
          const ref = `blob${blobs.size}`;
          const bytesBase64 = rpc.params.blobs[0].bytesBase64;
          blobs.set(ref, bytesBase64);
          result = {
            blobs: [
              {
                blobRef: ref,
                bytes: Buffer.from(bytesBase64, "base64").length,
              },
            ],
          };
          break;
        }
        case "vfs/snapshots/commit":
          committedManifest = rpc.params.manifest;
          result = { snapshotRef: "next" };
          break;
        case "vfs/workspaces/update":
          expect(rpc.params.expectedRevision).toBe(revision);
          if (race)
            return Response.json({
              id: rpc.id,
              error: {
                code: -32009,
                message: "conflict",
                data: { kind: "conflict" },
              },
            });
          if (rpc.params.displayName !== undefined) {
            expect(rpc.params.snapshotRef).toBe("snapshot");
            displayName = rpc.params.displayName;
          } else {
            if (!committedManifest)
              throw new Error("Expected a committed snapshot");
            manifest = committedManifest;
          }
          revision++;
          result = {
            workspace: {
              workspaceId: "docs",
              revision,
              displayName,
              headSnapshotRef: "snapshot",
            },
          };
          break;
        case "blobs/read":
          result = { bytesBase64: blobs.get(rpc.params.blobRef) };
          break;
        default:
          throw new Error(`Unexpected RPC ${rpc.method}`);
      }
      return Response.json({
        id: rpc.id,
        result: { result, notifications: [] },
      });
    }),
  );
});
afterEach(() => vi.unstubAllGlobals());

function app() {
  const app = new Hono<{ Variables: ApiVariables }>();
  app.route(
    "/",
    gatewayRoutes({
      env: {
        lightspeedApiUrl: "https://engine.example/rpc",
        lightspeedApiKey: "lsk_fixture",
      },
    } as AppContext),
  );
  return app;
}
const entries = [
  { kind: "directory", path: "reports/empty" },
  {
    kind: "file",
    path: "reports/résumé #1?.pdf",
    contentBase64: Buffer.from([0, 255, 12, 4]).toString("base64"),
    mediaType: "application/pdf",
  },
  {
    kind: "file",
    path: "reports/notes.txt",
    contentBase64: Buffer.from("notes").toString("base64"),
  },
];
async function upload(extra = {}, items = entries) {
  return app().request("/u/workspaces/docs/upload", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      expectedRevision: revision,
      entries: items,
      ...extra,
    }),
  });
}

it.each(["viewer", "contributor"])(
  "refuses workspace deletion by a %s before contacting the engine",
  async (role) => {
    identity.role = role;
    const response = await app().request("/u/workspaces/docs", {
      method: "DELETE",
    });
    expect(response.status).toBe(403);
    expect(calls).toEqual([]);
    expect(workspaceDeleted).toBe(false);
  },
);

it.each(["operator", "admin"])(
  "deletes a workspace as %s without deleting its snapshots or blobs",
  async (role) => {
    await upload();
    const original = structuredClone(manifest);
    const originalBlobs = new Map(blobs);
    calls = [];
    identity.role = role;
    const response = await app().request("/u/workspaces/docs", {
      method: "DELETE",
    });
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({
      workspace: { workspaceId: "docs" },
    });
    expect(workspaceDeleted).toBe(true);
    expect(calls).toEqual(["vfs/workspaces/delete"]);
    expect(manifest).toEqual(original);
    expect(blobs).toEqual(originalBlobs);
    expect(
      (await app().request("/u/workspaces/docs", { method: "DELETE" })).status,
    ).toBe(404);
  },
);

async function rename(path: string, name: string, expectedRevision = revision) {
  return app().request("/u/workspaces/docs/rename", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ path, name, expectedRevision }),
  });
}

async function renameWorkspace(name: string, expectedRevision = revision) {
  return app().request("/u/workspaces/docs", {
    method: "PATCH",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ displayName: name, expectedRevision }),
  });
}
it.each(["operator", "admin"])(
  "renames a workspace as %s without changing its ID or snapshot",
  async (role) => {
    await upload();
    const original = structuredClone(manifest);
    const originalBlobs = new Map(blobs);
    calls = [];
    identity.role = role;
    const response = await renameWorkspace("  Project notes 🗒  ");
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({
      workspace: {
        workspaceId: "docs",
        displayName: "Project notes 🗒",
        headSnapshotRef: "snapshot",
        revision: 2,
      },
    });
    expect(calls).toEqual(["vfs/workspaces/read", "vfs/workspaces/update"]);
    expect(manifest).toEqual(original);
    expect(blobs).toEqual(originalBlobs);
  },
);
it.each(["viewer", "contributor"])(
  "refuses workspace renaming by a %s",
  async (role) => {
    identity.role = role;
    expect((await renameWorkspace("New name")).status).toBe(403);
    expect(calls).toEqual([]);
  },
);
it("validates workspace names and rejects stale or racing renames", async () => {
  identity.role = "operator";
  for (const name of ["", "   ", "x".repeat(101)])
    expect((await renameWorkspace(name)).status).toBe(400);
  expect(calls).toEqual([]);
  expect((await renameWorkspace("New", 1)).status).toBe(409);
  expect(calls).not.toContain("vfs/workspaces/update");
  race = true;
  expect((await renameWorkspace("New")).status).toBe(409);
  expect(revision).toBe(0);
  expect(displayName).toBeUndefined();
});

it("renames files and folders atomically while preserving content, metadata and empty folders", async () => {
  await upload();
  const original = structuredClone(manifest);
  calls = [];
  expect((await rename("reports", "renamed #? 🗂")).status).toBe(200);
  expect(manifest.root.entries["renamed #? 🗂"]).toEqual(
    original.root.entries.reports,
  );
  expect(manifest.root.entries.reports).toBeUndefined();
  expect(manifest.totals).toEqual(original.totals);
  expect(revision).toBe(2);
  expect(calls.filter((call) => call === "vfs/workspaces/update")).toHaveLength(
    1,
  );
  expect(calls).not.toContain("blobs/put");
  const directory = manifest.root.entries["renamed #? 🗂"]!;
  if (directory.kind !== "directory") throw new Error("Expected directory");
  const file = structuredClone(directory.entries["résumé #1?.pdf"]);
  expect((await rename("renamed #? 🗂/résumé #1?.pdf", "new.pdf")).status).toBe(
    200,
  );
  const renamedDirectory = manifest.root.entries["renamed #? 🗂"]!;
  if (renamedDirectory.kind !== "directory")
    throw new Error("Expected directory");
  expect(renamedDirectory.entries["new.pdf"]).toEqual(file);
  expect(renamedDirectory.entries["résumé #1?.pdf"]).toBeUndefined();
  expect((await rename("renamed #? 🗂/empty", "__proto__")).status).toBe(200);
  const archive = await app().request(
    `/u/workspaces/docs/download?${new URLSearchParams({ path: "renamed #? 🗂" })}`,
  );
  const files = unzipSync(new Uint8Array(await archive.arrayBuffer()));
  expect(files["renamed #? 🗂/__proto__/"]).toEqual(new Uint8Array());
  expect(files["renamed #? 🗂/new.pdf"]).toEqual(
    new Uint8Array([0, 255, 12, 4]),
  );
});

it("refuses rename collisions, missing sources and unsafe names without changing the workspace", async () => {
  await upload();
  const original = structuredClone(manifest);
  calls = [];
  expect((await rename("reports/notes.txt", "empty")).status).toBe(409);
  expect((await rename("reports/empty", "notes.txt")).status).toBe(409);
  expect((await rename("missing", "new")).status).toBe(404);
  for (const name of [
    "",
    ".",
    "..",
    "../escape",
    "nested/name",
    "back\\slash",
    "bad\u0000name",
  ]) {
    expect((await rename("reports", name)).status).toBe(400);
  }
  expect((await rename("", "root")).status).toBe(400);
  expect(manifest).toEqual(original);
  expect(revision).toBe(1);
  expect(calls).not.toContain("vfs/snapshots/commit");
});

it("guards renames with contributor permissions and revision checks, including a racing update", async () => {
  await upload();
  const original = structuredClone(manifest);
  calls = [];
  identity.role = "viewer";
  expect((await rename("reports", "new")).status).toBe(403);
  expect(calls).toEqual([]);
  identity.role = "contributor";
  expect((await rename("reports", "new", 0)).status).toBe(409);
  expect(calls).not.toContain("vfs/snapshots/commit");
  race = true;
  expect((await rename("reports", "new")).status).toBe(409);
  expect(revision).toBe(1);
  expect(manifest).toEqual(original);
});

it("creates an empty folder without blobs and preserves it in workspace ZIP downloads", async () => {
  expect(
    (await upload({}, [{ kind: "directory", path: "empty" }])).status,
  ).toBe(200);
  expect(manifest.root.entries).toEqual({
    empty: { kind: "directory", entries: {} },
  });
  expect(manifest.totals).toEqual({ files: 0, bytes: 0 });
  expect(revision).toBe(1);
  expect(calls).not.toContain("blobs/put");
  const archive = await app().request("/u/workspaces/docs/download");
  expect(archive.status).toBe(200);
  const unzipped = unzipSync(new Uint8Array(await archive.arrayBuffer()));
  expect(unzipped["docs/empty/"]).toEqual(new Uint8Array());
});

it("uploads binary files and empty folders in one revision, then downloads files, folders and the workspace", async () => {
  expect((await upload()).status).toBe(200);
  expect(revision).toBe(1);
  expect(manifest.totals).toEqual({ files: 2, bytes: 9 });
  expect(calls.filter((call) => call === "vfs/workspaces/update")).toHaveLength(
    1,
  );
  identity.role = "viewer";
  const file = await app().request(
    `/u/workspaces/docs/download?${new URLSearchParams({ path: "reports/résumé #1?.pdf" })}`,
  );
  expect(file.status).toBe(200);
  expect(file.headers.get("content-disposition")).toContain(
    "r%C3%A9sum%C3%A9%20%231%3F.pdf",
  );
  expect(new Uint8Array(await file.arrayBuffer())).toEqual(
    new Uint8Array([0, 255, 12, 4]),
  );
  for (const path of ["reports", ""]) {
    const archive = await app().request(
      `/u/workspaces/docs/download?path=${path}`,
    );
    expect(archive.headers.get("content-type")).toBe("application/zip");
    const unzipped = unzipSync(new Uint8Array(await archive.arrayBuffer()));
    const prefix = path ? "reports" : "docs/reports";
    expect(unzipped[`${prefix}/empty/`]).toEqual(new Uint8Array());
    expect(unzipped[`${prefix}/résumé #1?.pdf`]).toEqual(
      new Uint8Array([0, 255, 12, 4]),
    );
    expect(new TextDecoder().decode(unzipped[`${prefix}/notes.txt`])).toBe(
      "notes",
    );
  }
});

it("rejects viewers before uploads or commits, including directory-only uploads", async () => {
  identity.role = "viewer";
  expect(
    (await upload({}, [{ kind: "directory", path: "empty" }])).status,
  ).toBe(403);
  expect(calls).toEqual([]);
});

it("detects stale revisions and path conflicts before writing blobs", async () => {
  expect((await upload({ expectedRevision: 12 })).status).toBe(409);
  expect(calls).toEqual(["vfs/workspaces/read"]);
  await upload();
  calls = [];
  expect((await upload()).status).toBe(409);
  expect(calls).not.toContain("blobs/put");
  expect((await upload({ replace: true })).status).toBe(200);
});

it("never commits a partial upload after a blob failure", async () => {
  failBlob = true;
  expect((await upload()).status).toBe(502);
  expect(revision).toBe(0);
  expect(calls).not.toContain("vfs/snapshots/commit");
});

it("uses the engine revision guard when another writer wins the race", async () => {
  race = true;
  const response = await upload();
  expect(response.status).toBe(409);
  expect(revision).toBe(0);
});

it.each(["../secret", "/absolute", "a//b", "a\\b", "a/./b", "a\u0000b"])(
  "rejects unsafe upload paths: %s",
  async (path) => {
    expect((await upload({}, [{ kind: "directory", path }])).status).toBe(400);
    expect(calls).toEqual([]);
  },
);

it("handles prototype-like filenames as ordinary own properties", async () => {
  expect(
    (
      await upload({}, [
        { kind: "file", path: "__proto__/constructor", contentBase64: "AA==" },
      ])
    ).status,
  ).toBe(200);
  const result = await app().request(
    "/u/workspaces/docs/download?path=__proto__%2Fconstructor",
  );
  expect(new Uint8Array(await result.arrayBuffer())).toEqual(
    new Uint8Array([0]),
  );
});

it("returns a missing-path error and exports an empty workspace as a valid ZIP", async () => {
  expect(
    (await app().request("/u/workspaces/docs/download?path=missing")).status,
  ).toBe(404);
  const result = await app().request("/u/workspaces/docs/download");
  expect(
    Object.keys(unzipSync(new Uint8Array(await result.arrayBuffer()))),
  ).toEqual(["docs/"]);
});
