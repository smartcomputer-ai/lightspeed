import { describe, expect, it } from "vitest";
import type { BotControllerSnapshot, BotSessionSnapshot, BotStateView, SessionSummaryView } from "@/api";
import { conversationTabs } from "./detail";
import { environmentSummary, guardrailsSummary, otherBotsSummary } from "./setup-summary";

function controller(partial: Partial<BotControllerSnapshot>): BotControllerSnapshot {
  return {
    mainSessionId: "bot:v1:triage",
    controllerStatus: "idle",
    setupStatus: "ready",
    enabled: true,
    closed: false,
    sessions: [{ sessionId: "bot:v1:triage", label: "main", kind: "main", busy: false, generation: 1 }],
    activeDeliveries: [],
    ...partial,
  };
}

function state(partial: Partial<BotControllerSnapshot>, descendants?: SessionSummaryView[]): BotStateView {
  return { controller: controller(partial), ...(descendants ? { descendants } : {}) };
}

const thread = (id: string, label: string, lastActiveAtMs: number): BotSessionSnapshot => ({
  sessionId: id,
  label,
  kind: "perKey",
  busy: false,
  generation: 1,
  lastActiveAtMs,
});

const subagent = (
  id: string,
  displayName: string,
  parentSessionId: string,
  updatedAtMs: number,
): SessionSummaryView => ({
  id,
  displayName,
  lifecycleStatus: "open",
  managed: true,
  createdAtMs: 0,
  updatedAtMs,
  retention: { rootSessionId: "bot:v1:triage" },
  origin: {
    kind: "subagent",
    parentSessionId,
    parentRunId: "run-1",
    rootSessionId: "bot:v1:triage",
    depth: 1,
    invocationId: "inv-1",
    agent: { profileId: "reviewer", revision: 1 },
    limits: { maxDepth: 1, maxDescendants: 4, maxConcurrent: 1, deadlineMs: 60_000 },
  },
});

describe("conversationTabs", () => {
  it("is just Main for a bot with one session", () => {
    const tabs = conversationTabs(state({}), undefined);
    expect(tabs.inline.map((tab) => tab.label)).toEqual(["Main"]);
    expect(tabs.overflow).toEqual([]);
  });
  it("keeps the most recent threads inline and folds the rest, sub-agents included", () => {
    const current = state(
      {
        sessions: [
          { sessionId: "bot:v1:triage", label: "main", kind: "main", busy: false, generation: 1 },
          thread("t-old", "PR-1", 1),
          thread("t-3", "PR-3", 3),
          thread("t-2", "PR-2", 2),
          thread("t-4", "PR-4", 4),
        ],
        activeDeliveries: [{ deliveryId: "d", seqs: [7], sessionId: "t-4", startedAtMs: 0 }],
      },
      [subagent("sub-1", "reviewer", "t-4", 5)],
    );
    const tabs = conversationTabs(current, undefined);
    expect(tabs.inline.map((tab) => tab.label)).toEqual(["Main", "PR-4", "PR-3", "PR-2"]);
    expect(tabs.inline[1]?.live).toBe(true);
    expect(tabs.overflow.map((tab) => tab.label)).toEqual(["PR-1", "reviewer"]);
    expect(tabs.overflow[1]?.hint).toBe("sub-agent of PR-4");
  });
  it("resolves a nested sub-agent's parent from descendant display names", () => {
    const parent = subagent("sub-parent-id", "planner", "bot:v1:triage", 4);
    const child = subagent("sub-child-id", "reviewer", "sub-parent-id", 5);
    const tabs = conversationTabs(state({}, [parent, child]), undefined);
    expect(tabs.overflow.find((tab) => tab.id === "sub-child-id")?.hint)
      .toBe("sub-agent of planner");
  });
  it("always shows the selected conversation inline", () => {
    const current = state({
      sessions: [
        { sessionId: "bot:v1:triage", label: "main", kind: "main", busy: false, generation: 1 },
        thread("t-1", "PR-1", 1),
        thread("t-2", "PR-2", 2),
        thread("t-3", "PR-3", 3),
        thread("t-4", "PR-4", 4),
      ],
    });
    const tabs = conversationTabs(current, "t-1");
    expect(tabs.inline.map((tab) => tab.id)).toContain("t-1");
    expect(tabs.overflow.map((tab) => tab.id)).not.toContain("t-1");
    const unknown = conversationTabs(current, "closed-subagent-session-id");
    expect(unknown.inline.at(-1)?.id).toBe("closed-subagent-session-id");
  });
});

describe("setup summaries", () => {
  it("reads guardrails as one line", () => {
    expect(
      guardrailsSummary({
        runsPerDay: 50,
        breaker: { fires: 20, windowMs: 600_000 },
        routedSessionCloseAfterMs: 7 * 86_400_000,
        selfConfig: true,
      }),
    ).toBe("50 runs a day · flood 20/10 min · threads close after 7d · can change own triggers");
    expect(
      guardrailsSummary({ runsPerDay: null, breaker: null, routedSessionCloseAfterMs: null, selfConfig: false }),
    ).toBe("no daily limit · threads kept");
    expect(otherBotsSummary(true, ["release-shepherd"])).toBe("can send · receives from release-shepherd");
    expect(otherBotsSummary(false, "off")).toBe("cannot send · receives from nobody");
  });
  it("names the environment", () => {
    expect(environmentSummary(undefined)).toBe("No environment");
    expect(environmentSummary({ type: "existing", environmentId: "env-1" })).toBe("env-1");
  });
});
