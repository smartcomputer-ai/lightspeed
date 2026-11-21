import { describe, expect, it } from "vitest";
import {
  DEFAULT_SESSION_LIST_PREFERENCES,
  metadataFilterFromSearchParams,
  parseMetadataPair,
  parseSessionListPreferences,
  parseSessionMetadataFilter,
  hiddenSessionsNote,
  searchParamsWithMetadataFilter,
} from "./list-preferences";

describe("session list preferences", () => {
  it("names what the list's scope hides", () => {
    expect(hiddenSessionsNote(DEFAULT_SESSION_LIST_PREFERENCES)).toBe("Managed sessions are hidden.");
    expect(hiddenSessionsNote({ showClosed: false, showSubagents: true, showControlledSessions: false }))
      .toBe("Closed and managed sessions are hidden.");
    expect(hiddenSessionsNote({ showClosed: false, showSubagents: false, showControlledSessions: false }))
      .toBe("Closed, sub-agent and managed sessions are hidden.");
    expect(hiddenSessionsNote({ showClosed: true, showSubagents: true, showControlledSessions: true })).toBeNull();
  });

  it("reads valid values and falls back field by field", () => {
    expect(parseSessionListPreferences({
      showClosed: false,
      showSubagents: "yes",
      showSessionIds: true,
      metadataKeys: [" campaign ", "owner", "campaign", 12, ""],
    })).toEqual({
      showClosed: false,
      showSubagents: true,
      // Absent: managed work stays on its manager's page.
      showControlledSessions: false,
      showSessionIds: true,
      metadataKeys: ["campaign", "owner"],
    });
    expect(parseSessionListPreferences("not an object")).toBeUndefined();
    expect(parseSessionListPreferences([])).toBeUndefined();
  });

  it("keeps only string metadata filter values", () => {
    expect(parseSessionMetadataFilter({ campaign: "nightly", count: 3, "": "x" })).toEqual({ campaign: "nightly" });
    expect(parseSessionMetadataFilter(["campaign"])).toBeUndefined();
  });
});

describe("session metadata filter query", () => {
  it("parses values containing equals signs", () => {
    expect(parseMetadataPair(" campaign = terminal=bench "))
      .toEqual({ key: "campaign", value: "terminal=bench" });
    expect(parseMetadataPair("present-key")).toEqual({ key: "present-key", value: "" });
    expect(parseMetadataPair("present-key=")).toEqual({ key: "present-key", value: "" });
    expect(parseMetadataPair("=missing-key")).toBeNull();
  });

  it("round-trips repeated metadata parameters while preserving other query state", () => {
    const params = searchParamsWithMetadataFilter(
      new URLSearchParams("view=tree&metadata=old=value"),
      { source: "harbor", campaign: "nightly" },
    );
    expect(params.get("view")).toBe("tree");
    expect(params.getAll("metadata")).toEqual(["source=harbor", "campaign=nightly"]);
    expect(metadataFilterFromSearchParams(params))
      .toEqual({ source: "harbor", campaign: "nightly" });
  });

  it("round-trips presence-only metadata filters", () => {
    const params = searchParamsWithMetadataFilter(new URLSearchParams(), { campaign: "" });
    expect(params.getAll("metadata")).toEqual(["campaign"]);
    expect(metadataFilterFromSearchParams(params)).toEqual({ campaign: "" });
  });
});
