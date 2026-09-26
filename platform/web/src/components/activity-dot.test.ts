import { expect, it } from "vitest";
import { activityLabel, activityTone, foldActivity } from "./activity-dot";

it("folds many sessions into what needs a person first, then work", () => {
  expect(foldActivity([])).toBe("idle");
  expect(foldActivity(["idle", "working", "idle"])).toBe("working");
  expect(foldActivity(["working", "waiting", undefined])).toBe("waiting");
});

it("names and colours only what is happening", () => {
  expect(activityTone("working")).toBe("live");
  expect(activityTone("waiting")).toBe("waiting");
  expect(activityTone("waiting", true)).toBe("closed");
  expect(activityLabel("waiting")).toBe("Waiting for approval");
  expect(activityLabel("idle")).toBeNull();
});
