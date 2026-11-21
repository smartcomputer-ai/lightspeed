import { describe, expect, it } from "vitest";
import { sessionMenuMetadataEntries } from "./session-menu-details";

describe("session menu metadata", () => {
  it("sorts metadata without shortening its values", () => {
    const long = "campaign-terminal-bench-lightspeed-rerun-hosted-20260903-193618";
    expect(sessionMenuMetadataEntries({ workflowRunId: long, agent: "lightspeed" })).toEqual([
      ["agent", "lightspeed"],
      ["workflowRunId", long],
    ]);
  });
});
