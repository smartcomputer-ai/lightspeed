import { describe, expect, it } from "vitest";
import { idlePolicyProblem, idleStagesFor } from "./idle-policy-fields";

describe("idle policy fields", () => {
  it("offers only the stages the template's power states support, close always", () => {
    expect(idleStagesFor(["running", "paused", "stopped"]).map((stage) => stage.key)).toEqual([
      "pauseAfterMs",
      "stopAfterMs",
      "closeAfterMs",
    ]);
    expect(idleStagesFor([]).map((stage) => stage.key)).toEqual(["closeAfterMs"]);
  });

  it("requires at least one stage and non-decreasing thresholds", () => {
    expect(idlePolicyProblem(undefined)).toMatch(/at least one stage/);
    expect(idlePolicyProblem({})).toMatch(/at least one stage/);
    expect(idlePolicyProblem({ pauseAfterMs: 60_000, stopAfterMs: 30_000 })).toMatch(/must not decrease/);
    expect(idlePolicyProblem({ pauseAfterMs: 60_000, stopAfterMs: 60_000 })).toBeNull();
    expect(idlePolicyProblem({ closeAfterMs: 3_600_000 })).toBeNull();
  });
});
