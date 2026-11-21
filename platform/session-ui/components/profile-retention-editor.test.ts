import { describe, expect, it } from "vitest";
import { retentionDaysValue } from "./profile-retention-editor";

describe("profile retention editor", () => {
  it("converts days to the close-relative millisecond policy", () => {
    expect(retentionDaysValue("14")).toEqual({
      deleteAfterCloseMs: 1_209_600_000,
      error: null,
    });
  });

  it("uses an empty value to keep sessions until manual deletion", () => {
    expect(retentionDaysValue("")).toEqual({
      deleteAfterCloseMs: undefined,
      error: null,
    });
  });

  it("rejects non-positive and overlong policies", () => {
    expect(retentionDaysValue("0").error).toContain("positive");
    expect(retentionDaysValue(String(101 * 365)).error).toContain("100 years");
  });
});
