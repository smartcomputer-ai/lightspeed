import { describe, expect, it } from "vitest";
import { documentByteLimit, documentMime, mediaKindForMime } from "../src/media.js";

describe("documentMime", () => {
  it("prefers the file extension over the reported mime", () => {
    // Channels report generic mimes for text files.
    expect(documentMime("notes.md", "application/octet-stream")).toBe("text/markdown");
    expect(documentMime("data.csv", "text/comma-separated-values")).toBe("text/csv");
    expect(documentMime("report.pdf", undefined)).toBe("application/pdf");
  });

  it("falls back to an allowed reported mime", () => {
    expect(documentMime("unknown.bin", "application/pdf")).toBe("application/pdf");
    expect(documentMime(undefined, "text/plain; charset=utf-8")).toBe("text/plain");
  });

  it("rejects unsupported document types", () => {
    expect(documentMime("slides.pptx", "application/vnd.ms-powerpoint")).toBeNull();
    expect(documentMime("archive.zip", "application/zip")).toBeNull();
    expect(documentMime(undefined, undefined)).toBeNull();
  });
});

describe("documentByteLimit", () => {
  it("allows larger PDFs than text documents", () => {
    expect(documentByteLimit("application/pdf")).toBeGreaterThan(
      documentByteLimit("text/markdown"),
    );
  });
});

describe("mediaKindForMime", () => {
  it("classifies images and documents", () => {
    expect(mediaKindForMime("image/jpeg")).toBe("image");
    expect(mediaKindForMime("application/pdf")).toBe("document");
    expect(mediaKindForMime("text/markdown")).toBe("document");
  });
});
