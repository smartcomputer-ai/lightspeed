import { expect, it } from "vitest";
import { appHref, blobDigest, blobHref, blobView } from "./blob-view";

const digest = "0123456789abcdef".repeat(4);
const bytes = (...values: number[]) => new Uint8Array(values);
const text = (value: string) => new TextEncoder().encode(value);

it("addresses blobs by digest within the universe, with hints in the query", () => {
  expect(blobDigest(`sha256:${digest}`)).toBe(digest);
  expect(blobDigest("blob:demo")).toBeNull();
  expect(blobHref("acme", `sha256:${digest}`)).toBe(`/u/acme/blobs/${digest}`);
  expect(blobHref("acme", `sha256:${digest}`, { name: "a b.md", type: "text/markdown", session: "s1" }))
    .toBe(`/u/acme/blobs/${digest}?name=a+b.md&type=text%2Fmarkdown&session=s1`);
  expect(blobHref("acme", "sha256:short")).toBeNull();
  // New-tab links carry the app's base path; tests run at "/".
  expect(appHref("/u/acme/blobs/x")).toBe(`${import.meta.env.BASE_URL.replace(/\/$/, "")}/u/acme/blobs/x`);
});

it("trusts the bytes over the claimed type", () => {
  expect(blobView(bytes(0x89, 0x50, 0x4e, 0x47, 0x0d), "text/plain")).toEqual({ kind: "image", mime: "image/png" });
  expect(blobView(text("%PDF-1.7"), undefined)).toEqual({ kind: "pdf", mime: "application/pdf" });
  expect(blobView(text("plain words"), "image/png")).toEqual({ kind: "text", mime: "text/plain", text: "plain words" });
  expect(blobView(bytes(0xff, 0xfe, 0x00, 0x81), "application/zip")).toEqual({ kind: "binary", mime: "application/octet-stream" });
});

it("shows text exactly as stored, including markup that could run as a page", () => {
  const markdown = "# Title\n\n* item";
  expect(blobView(text(markdown), "text/markdown")).toEqual({ kind: "text", mime: "text/markdown", text: markdown });
  const json = '{"a":1}';
  expect(blobView(text(json), "application/json")).toEqual({ kind: "text", mime: "application/json", text: json });
  const svg = '<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>';
  expect(blobView(text(svg), "image/svg+xml")).toEqual({ kind: "text", mime: "text/plain", text: svg });
});
