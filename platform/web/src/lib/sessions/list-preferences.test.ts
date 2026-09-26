import { describe, expect, it } from "vitest";
import {
  DEFAULT_SESSION_LIST_PREFERENCES,
  metadataFilterFromSearchParams,
  parseMetadataPair,
  readSessionMetadataFilter,
  readSessionListPreferences,
  hiddenSessionsNote,
  searchParamsWithMetadataFilter,
  writeSessionMetadataFilter,
  writeSessionListPreferences,
} from "./list-preferences";

describe("session list preferences", () => {
  it("names what the list's scope hides", () => {
    expect(hiddenSessionsNote(DEFAULT_SESSION_LIST_PREFERENCES)).toBe("Managed sessions are hidden.");
    expect(hiddenSessionsNote({ showClosed: false, showSubagents: true, showManagedSessions: false }))
      .toBe("Closed and managed sessions are hidden.");
    expect(hiddenSessionsNote({ showClosed: false, showSubagents: false, showManagedSessions: false }))
      .toBe("Closed, sub-agent and managed sessions are hidden.");
    expect(hiddenSessionsNote({ showClosed: true, showSubagents: true, showManagedSessions: true })).toBeNull();
  });

  it("reads valid values and falls back field by field", () => {
    expect(readSessionListPreferences("universe-a", {
      getItem: () => JSON.stringify({
        showClosed: false,
        showSubagents: "yes",
        showSessionIds: true,
        metadataKeys: [" campaign ", "owner", "campaign", 12, ""],
      }),
    })).toEqual({
      showClosed: false,
      showSubagents: true,
      // Absent: managed work stays on its manager's page.
      showManagedSessions: false,
      showSessionIds: true,
      metadataKeys: ["campaign", "owner"],
    });
    expect(readSessionListPreferences("universe-a", { getItem: () => "not-json" }))
      .toEqual(DEFAULT_SESSION_LIST_PREFERENCES);
  });

  it("stores visibility preferences per universe", () => {
    const values = new Map<string, string>();
    const storage = {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => { values.set(key, value); },
    };
    writeSessionListPreferences(
      "universe-a",
      { showClosed: false, showSubagents: true, showManagedSessions: true, showSessionIds: false, metadataKeys: ["campaign"] },
      storage,
    );
    writeSessionListPreferences(
      "universe-b",
      { showClosed: true, showSubagents: false, showManagedSessions: false, showSessionIds: true, metadataKeys: ["owner"] },
      storage,
    );
    expect(readSessionListPreferences("universe-a", storage))
      .toEqual({
        showClosed: false,
        showSubagents: true,
        showManagedSessions: true,
        showSessionIds: false,
        metadataKeys: ["campaign"],
      });
    expect(readSessionListPreferences("universe-b", storage))
      .toEqual({
        showClosed: true,
        showSubagents: false,
        showManagedSessions: false,
        showSessionIds: true,
        metadataKeys: ["owner"],
      });
  });

  it("stores metadata filters per universe", () => {
    const values = new Map<string, string>();
    const storage = {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => { values.set(key, value); },
    };
    writeSessionMetadataFilter("universe-a", { campaign: "nightly" }, storage);
    writeSessionMetadataFilter("universe-b", { source: "bot" }, storage);
    expect(readSessionMetadataFilter("universe-a", storage)).toEqual({ campaign: "nightly" });
    expect(readSessionMetadataFilter("universe-b", storage)).toEqual({ source: "bot" });
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
