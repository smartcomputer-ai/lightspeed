import { expect, it } from "vitest";
import { unzipSync } from "fflate";
import { createDemoStore } from "./fixtures";
import { createDemoRouter } from "./router";
import { SOFTWARE_FACTORY_UNIVERSE_ID } from "./fixtures/software-factory";

it("removes a workspace from the demo list and refuses further workspace reads", async () => {
  const store = createDemoStore();
  const app = createDemoRouter(store);
  const base = `/api/v1/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}/workspaces`;
  const created = await app.request(base, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ workspaceId: "delete-test" }),
  });
  expect(created.status).toBe(201);
  const original = await created.json();
  const rename = (displayName: string, expectedRevision: number) =>
    app.request(`${base}/delete-test`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ displayName, expectedRevision }),
    });
  const renamed = await rename("  Renamed workspace  ", 0);
  expect(renamed.status).toBe(200);
  expect(await renamed.json()).toMatchObject({
    workspace: {
      workspaceId: "delete-test",
      displayName: "Renamed workspace",
      headSnapshotRef: original.headSnapshotRef,
      revision: 1,
    },
  });
  expect((await rename("Stale", 0)).status).toBe(409);
  expect((await rename(" ", 1)).status).toBe(400);
  const deleted = await app.request(`${base}/delete-test`, {
    method: "DELETE",
  });
  expect(deleted.status).toBe(200);
  expect(await deleted.json()).toMatchObject({
    workspace: { workspaceId: "delete-test" },
  });
  const rows = await (await app.request(base)).json();
  expect(
    rows.some(
      (row: { workspaceId: string }) => row.workspaceId === "delete-test",
    ),
  ).toBe(false);
  expect((await app.request(`${base}/delete-test/tree`)).status).toBe(404);
  expect(
    (await app.request(`${base}/delete-test`, { method: "DELETE" })).status,
  ).toBe(404);
});

it("round-trips uploaded folders and binary files through the demo routes", async () => {
  const app = createDemoRouter(createDemoStore());
  const base = `/api/v1/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}/workspaces`;
  const post = (path: string, body: unknown) =>
    app.request(path, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
  expect((await post(base, { workspaceId: "upload-test" })).status).toBe(201);
  const upload = {
    expectedRevision: 0,
    entries: [
      { kind: "directory", path: "folder/empty" },
      { kind: "file", path: "folder/binary.dat", contentBase64: "AP8=" },
    ],
  };
  expect((await post(`${base}/upload-test/upload`, upload)).status).toBe(200);
  expect((await post(`${base}/upload-test/upload`, upload)).status).toBe(409);
  const response = await app.request(
    `${base}/upload-test/download?path=folder`,
  );
  const entries = unzipSync(new Uint8Array(await response.arrayBuffer()));
  expect(entries["folder/empty/"]).toEqual(new Uint8Array());
  expect(entries["folder/binary.dat"]).toEqual(new Uint8Array([0, 255]));
  expect(
    (
      await post(`${base}/upload-test/rename`, {
        path: "folder",
        name: "renamed",
        expectedRevision: 1,
      })
    ).status,
  ).toBe(200);
  expect(
    (
      await post(`${base}/upload-test/rename`, {
        path: "renamed/empty",
        name: "binary.dat",
        expectedRevision: 2,
      })
    ).status,
  ).toBe(409);
  expect(
    (
      await post(`${base}/upload-test/rename`, {
        path: "renamed",
        name: "stale",
        expectedRevision: 1,
      })
    ).status,
  ).toBe(409);
  expect(
    (
      await post(`${base}/upload-test/rename`, {
        path: "renamed",
        name: "../escape",
        expectedRevision: 2,
      })
    ).status,
  ).toBe(400);
  expect(
    (
      await post(`${base}/upload-test/rename`, {
        path: "renamed/binary.dat",
        name: "new.dat",
        expectedRevision: 2,
      })
    ).status,
  ).toBe(200);
  const renamedArchive = await app.request(
    `${base}/upload-test/download?path=renamed`,
  );
  const renamedEntries = unzipSync(
    new Uint8Array(await renamedArchive.arrayBuffer()),
  );
  expect(renamedEntries["renamed/empty/"]).toEqual(new Uint8Array());
  expect(renamedEntries["renamed/new.dat"]).toEqual(new Uint8Array([0, 255]));
  expect(
    (
      await app.request(
        `${base}/upload-test/entries?path=renamed&expectedRevision=3`,
        { method: "DELETE" },
      )
    ).status,
  ).toBe(200);
  const tree = await (await app.request(`${base}/upload-test/tree`)).json();
  expect(tree.manifest.root.entries).toEqual({});
  expect(tree.manifest.totals).toEqual({ files: 0, bytes: 0 });
});

it("moves demo entries atomically and rejects stale, conflicting and recursive moves", async () => {
  const app = createDemoRouter(createDemoStore());
  const base = `/api/v1/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}/workspaces`;
  const post = (path: string, body: unknown) => app.request(path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  expect((await post(base, { workspaceId: "move-test" })).status).toBe(201);
  const workspace = `${base}/move-test`;
  expect((await post(`${workspace}/upload`, {
    expectedRevision: 0,
    entries: [
      { kind: "directory", path: "folder/empty" },
      { kind: "directory", path: "archive" },
      { kind: "file", path: "folder/binary.dat", contentBase64: "AP8=" },
    ],
  })).status).toBe(200);
  const move = (path: string, destination: string, expectedRevision = 1) =>
    post(`${workspace}/move`, { path, destination, expectedRevision });
  expect((await move("folder", "archive/folder", 0)).status).toBe(409);
  expect((await move("folder", "folder/empty/folder")).status).toBe(400);
  expect((await move("folder", "archive")).status).toBe(409);
  expect((await move("folder", "archive/folder")).status).toBe(200);
  expect((await move("archive/folder/binary.dat", "binary.dat", 2)).status).toBe(200);
  const tree = await (await app.request(`${workspace}/tree`)).json();
  expect(tree.workspace.revision).toBe(3);
  expect(tree.manifest.root.entries.folder).toBeUndefined();
  expect(tree.manifest.root.entries.archive.entries.folder.entries.empty).toEqual({ kind: "directory", entries: {} });
  expect(tree.manifest.totals).toEqual({ files: 1, bytes: 2 });
  const response = await app.request(`${workspace}/download?path=binary.dat`);
  expect(new Uint8Array(await response.arrayBuffer())).toEqual(new Uint8Array([0, 255]));
});
