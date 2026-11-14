import { expect, it } from "vitest";
import type { VfsFileEntry } from "@/api";
import { fileMetaItems, fileTypeName, lineCount } from "./workspace-file-meta";

const file = (overrides: Partial<VfsFileEntry> = {}): VfsFileEntry => ({
  kind: "file",
  blob_ref: `sha256:${"ab".repeat(32)}`,
  size_bytes: 2048,
  executable: false,
  ...overrides,
});

it.each([
  ["main.ts", undefined, "TypeScript"],
  ["App.TSX", undefined, "TypeScript JSX"],
  ["notes.md", "text/markdown", "Markdown"],
  ["Dockerfile", undefined, "Dockerfile"],
  ["photo", "image/heic", "HEIC image"],
  ["clip.bin", "video/x-matroska", "MATROSKA video"],
  ["readme", "text/plain", "Plain text"],
  ["data.parquet", "application/octet-stream", "PARQUET file"],
  ["noextension", undefined, "File"],
])("names %s (%s) as %s", (name, mediaType, expected) => {
  expect(fileTypeName(name, mediaType)).toBe(expected);
});

it.each([
  ["", 0],
  ["one", 1],
  ["one\n", 1],
  ["one\ntwo", 2],
  ["one\ntwo\n", 2],
  ["\n", 1],
])("counts the lines of %j as %i", (text, lines) => {
  expect(lineCount(text)).toBe(lines);
});

it("shows a text file's type, size with lines, hash and revision, all on the left", () => {
  const items = fileMetaItems({ name: "main.ts", file: file(), text: "a\nb\nc\n", revision: 12, blobViewerHref: "/app/u/x/blobs/ab" });
  expect(items.map((item) => item.key)).toEqual(["type", "size", "content", "revision"]);
  expect(items.find((item) => item.key === "type")!.text).toBe("TypeScript");
  expect(items.find((item) => item.key === "size")!.value).toBe("2 KB · 3 lines");
  expect(items.find((item) => item.key === "revision")!.value).toBe("r12");
  expect(items.every((item) => item.align === undefined)).toBe(true);
});

it("gives a binary file its size alone and marks an executable one", () => {
  const items = fileMetaItems({ name: "run", file: file({ executable: true, size_bytes: 10 }), text: null, revision: undefined, blobViewerHref: null });
  expect(items.map((item) => item.key)).toEqual(["type", "size", "executable", "content"]);
  expect(items.find((item) => item.key === "size")!.value).toBe("10 B");
});
