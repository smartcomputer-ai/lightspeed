import { expect, it } from "vitest";
import { unzipSync } from "fflate";
import { createDemoStore } from "./fixtures";
import { createDemoRouter } from "./router";
import { SOFTWARE_FACTORY_UNIVERSE_ID } from "./fixtures/software-factory";

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
