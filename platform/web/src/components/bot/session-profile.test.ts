import { describe, expect, it } from "vitest";
import { mergeSessionProfileFields } from "./session-profile";

describe("bot session profile saves", () => {
  it("overlays edited fields onto the latest document", () => {
    expect(mergeSessionProfileFields({
      profileId: "triage",
      revision: 4,
      createdAtMs: 10,
      updatedAtMs: 20,
      description: "shared profile",
      config: { features: { environments: { environments: [{ environmentId: "old-box", access: "read", default: true }] } } },
      metadata: { owner: "ops" },
    }, {
      config: { features: { environments: { environments: [{ environmentId: "new-box", access: "jobs", default: true }] } } },
      retention: { deleteAfterCloseMs: 86_400_000 },
    })).toEqual({
      profileId: "triage",
      revision: 4,
      description: "shared profile",
      config: { features: { environments: { environments: [{ environmentId: "new-box", access: "jobs", default: true }] } } },
      metadata: { owner: "ops" },
      retention: { deleteAfterCloseMs: 86_400_000 },
    });
  });

  it("clears optional fields without disturbing unrelated profile data", () => {
    expect(mergeSessionProfileFields({
      profileId: "triage",
      revision: 2,
      instructions: { type: "text", text: "old" },
      config: { features: { environments: { environments: [{ environmentId: "ops-box", access: "exec", default: true }] } } },
      metadata: { team: "ops" },
    }, {
      instructions: undefined,
      config: undefined,
    })).toEqual({
      profileId: "triage",
      revision: 2,
      metadata: { team: "ops" },
    });
  });
});
