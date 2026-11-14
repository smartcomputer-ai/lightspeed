import { expect, it } from "vitest";
import { activityLabel, activityTone, foldActivity, sessionDotState, type SessionDotInput } from "./activity-dot";

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

const session = (fields: Partial<SessionDotInput> = {}): SessionDotInput =>
  ({ activity: "idle", lifecycleStatus: "open", ...fields });
const failedRun = { runId: "r1", status: "failed" as const, finishedAtMs: 1 };

it("marks a session's dot by what is most urgent", () => {
  expect(sessionDotState(session())).toEqual({ tone: "idle", label: null, hint: null });
  expect(sessionDotState(session({ activity: "working" }))).toMatchObject({ tone: "live", label: "Working" });
  expect(sessionDotState(session({ activity: "waiting" }))).toMatchObject({ tone: "waiting", label: "Waiting for approval" });
  // A run parked on work it started is neither working nor waiting on a person.
  expect(sessionDotState(session({ activity: "working", activityPhase: "awaiting" }))).toMatchObject({ tone: "awaiting", label: "Awaiting" });
  expect(sessionDotState(session({ activity: "waiting", activityPhase: "awaiting" }))).toMatchObject({ tone: "waiting" });
});

it("marks a stopped workflow and a failed last run, a stopped workflow first", () => {
  expect(sessionDotState(session({ lastRun: failedRun }))).toMatchObject({ tone: "attention", label: "Last run failed" });
  expect(sessionDotState(session({ lastRun: { ...failedRun, status: "completed" } }))).toMatchObject({ tone: "idle" });
  // A run under way says more than how the one before it ended.
  expect(sessionDotState(session({ activity: "working", lastRun: failedRun }))).toMatchObject({ tone: "live" });
  expect(sessionDotState(session({ activity: "working", executionFailure: "failed", lastRun: failedRun })))
    .toMatchObject({ tone: "attention", label: "Stopped" });
  expect(sessionDotState(session({ lifecycleStatus: "closed", executionFailure: "failed", lastRun: failedRun })))
    .toEqual({ tone: "closed", label: null, hint: null });
});
