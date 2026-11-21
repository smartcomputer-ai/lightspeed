import { describe, expect, it } from "vitest";
import { metadataToRows, rowsToMetadata, sameMetadata } from "./metadata-editor";

describe("metadata editor rows", () => {
  it("round-trips a map through rows", () => {
    const metadata = { job: "nightly", source: "harbor" };
    expect(rowsToMetadata(metadataToRows(metadata))).toEqual(metadata);
  });

  it("drops incomplete rows, trims, and lets the last duplicate win", () => {
    expect(
      rowsToMetadata([
        { key: " job ", value: " nightly " },
        { key: "", value: "orphan" },
        { key: "owner", value: "  " },
        { key: "job", value: "weekly" },
      ]),
    ).toEqual({ job: "weekly" });
  });

  it("compares maps by content", () => {
    expect(sameMetadata({ a: "1", b: "2" }, { b: "2", a: "1" })).toBe(true);
    expect(sameMetadata({ a: "1" }, { a: "2" })).toBe(false);
    expect(sameMetadata({ a: "1" }, {})).toBe(false);
  });
});
