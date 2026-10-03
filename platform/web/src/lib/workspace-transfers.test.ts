// @vitest-environment jsdom
import { expect, it } from "vitest";
import {
  droppedEntries,
  selectedFiles,
  uploadPayload,
  validateUploadEntries,
  directoryEntries,
  type UploadDirectoryHandle,
} from "./workspace-transfers";

it("preserves empty folders from the native directory picker", async () => {
  const empty: UploadDirectoryHandle = {
    kind: "directory",
    name: "empty",
    async *values() {},
  };
  const root: UploadDirectoryHandle = {
    kind: "directory",
    name: "root",
    async *values() {
      yield empty;
    },
  };
  expect(await directoryEntries(root)).toEqual([
    { kind: "directory", path: "root" },
    { kind: "directory", path: "root/empty" },
  ]);
});

it("retains picker paths and binary bytes, including URL-special characters", async () => {
  const file = new File([new Uint8Array([0, 255, 1])], "résumé #1?.pdf", {
    type: "application/pdf",
  });
  Object.defineProperty(file, "webkitRelativePath", {
    value: "docs/résumé #1?.pdf",
  });
  const payload = await uploadPayload(
    selectedFiles([file]),
    "target",
    7,
    false,
    () => {},
  );
  expect(payload).toEqual({
    expectedRevision: 7,
    replace: false,
    entries: [
      {
        kind: "file",
        path: "target/docs/résumé #1?.pdf",
        contentBase64: "AP8B",
        mediaType: "application/pdf",
      },
    ],
  });
});

function directory(name: string, batches: unknown[][]) {
  return {
    name,
    isDirectory: true,
    isFile: false,
    createReader: () => ({
      readEntries: (callback: (value: unknown[]) => void) =>
        callback(batches.shift() ?? []),
    }),
  };
}
it("reads every directory batch and preserves empty folders on drop", async () => {
  const file = new File(["hello"], "hello.txt");
  const entry = directory("docs", [
    [
      {
        name: "hello.txt",
        isFile: true,
        isDirectory: false,
        file: (callback: (file: File) => void) => callback(file),
      },
    ],
    [directory("empty", [[]])],
    [],
  ]);
  const result = await droppedEntries({
    items: [
      { kind: "file", webkitGetAsEntry: () => entry, getAsFile: () => null },
    ],
  } as unknown as DataTransfer);
  expect(result.map(({ kind, path }) => ({ kind, path }))).toEqual([
    { kind: "directory", path: "docs" },
    { kind: "file", path: "docs/hello.txt" },
    { kind: "directory", path: "docs/empty" },
  ]);
});

it("falls back to files where directory entries are unavailable", async () => {
  const file = new File(["hi"], "hello.txt");
  expect(
    await droppedEntries({
      items: [{ kind: "file", getAsFile: () => file }],
    } as unknown as DataTransfer),
  ).toEqual([{ kind: "file", path: "hello.txt", file }]);
});

it("rejects unreadable drops, duplicates and excessive sizes before uploading", async () => {
  await expect(
    droppedEntries({
      items: [{ kind: "file", getAsFile: () => null }],
    } as unknown as DataTransfer),
  ).rejects.toThrow("could not read");
  expect(() =>
    validateUploadEntries([
      { kind: "directory", path: "a" },
      { kind: "directory", path: "a" },
    ]),
  ).toThrow("Duplicate");
  const file = new File([], "large");
  Object.defineProperty(file, "size", { value: 33 * 1024 * 1024 });
  expect(() => validateUploadEntries(selectedFiles([file]))).toThrow("32 MiB");
});
