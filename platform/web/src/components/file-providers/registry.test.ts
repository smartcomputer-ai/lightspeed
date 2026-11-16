import { expect, it } from "vitest";
import { canEdit, fileFacts, providerFor } from "./registry";

const text = (value: string) => new TextEncoder().encode(value);
const winner = (path: string, mediaType: string | undefined, head: Uint8Array) =>
  providerFor(fileFacts(path, { size_bytes: head.length, ...(mediaType ? { media_type: mediaType } : {}) }, head)).id;

it.each([
  // Markdown by media type, or by extension with a text type or none.
  ["notes.md", "text/markdown", text("# Notes"), "markdown"],
  ["README.md", undefined, text("# Readme"), "markdown"],
  ["notes.markdown", "text/plain", text("# Notes"), "markdown"],
  // Text and code: by media type, extension, or the absence of NUL bytes.
  ["notes.txt", "text/plain", text("# Not Markdown"), "text"],
  ["src/index.ts", undefined, text("export {}"), "text"],
  ["config.json", "application/json", text("{}"), "text"],
  ["Makefile", undefined, text("all:\n"), "text"],
  // Images and PDFs, by type, extension, or the PDF signature.
  ["logo.png", "image/png", new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0]), "image"],
  ["chart.svg", undefined, text("<svg/>"), "image"],
  ["paper.pdf", undefined, text("%PDF-1.7"), "pdf"],
  ["download", "application/octet-stream", text("%PDF-1.4"), "pdf"],
  // Anything else: the fallback.
  ["deck.pptx", "application/vnd.openxmlformats-officedocument.presentationml.presentation", new Uint8Array([0x50, 0x4b, 3, 4, 0]), "fallback"],
  ["blob.bin", undefined, new Uint8Array([1, 0, 2]), "fallback"],
])("%s (%s) is shown by the %s provider", (path, mediaType, head, expected) => {
  expect(winner(path, mediaType, head)).toBe(expected);
});

it("offers editing only where a provider has an editable mode", () => {
  const provider = (path: string, head: Uint8Array) => providerFor(fileFacts(path, null, head));
  expect(canEdit(provider("notes.md", text("# Notes")))).toBe(true);
  expect(canEdit(provider("notes.txt", text("plain")))).toBe(true);
  expect(canEdit(provider("logo.png", new Uint8Array([0x89, 0x50])))).toBe(false);
  expect(canEdit(provider("blob.bin", new Uint8Array([0])))).toBe(false);
  // Markdown has two modes, so a toggle; the rest have one.
  expect(provider("notes.md", text("# Notes")).modes.map((mode) => mode.id)).toEqual(["preview", "source"]);
  expect(provider("notes.txt", text("plain")).modes).toHaveLength(1);
});
