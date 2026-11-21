import { describe, expect, it } from "vitest";
import { sessionCreateSchema, sessionListQuery } from "./collection.js";

describe("session creation setup", () => {
  it("accepts profile-based creation without an environment override", () => {
    const request = { profile: { kind: "named", profileId: "developer" } };
    expect(sessionCreateSchema.parse(request)).toEqual(request);
  });

  it.each([{ type: "none" }, { type: "existing", environmentId: "runner" }, null])(
    "rejects the removed environment override: %j", (environment) => {
      expect(sessionCreateSchema.safeParse({
        profile: { kind: "named", profileId: "developer" }, environment,
      }).success).toBe(false);
    },
  );
});

it("builds the runtime list filters and bounds the requested page", () => {
  const query = sessionListQuery("https://host/sessions?limit=500&cursor=abc&closed=false&metadata=team%3Dops&metadata=origin&subagent=no");
  expect(query).toMatchObject({ limit: 200, cursor: "abc", closed: false, metadata: { team: "ops", origin: "" } });
  expect(query.subagent).toBeUndefined();
});
