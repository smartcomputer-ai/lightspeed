import { describe, expect, it } from "vitest";
import type { BotControllerSnapshot, BotSessionSnapshot, BotStateView, SessionSummaryView } from "@/api";
import { botConversations } from "./conversations";
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

function state(partial: Partial<BotControllerSnapshot>, sessions?: SessionSummaryView[]): BotStateView {
  return { controller: controller(partial), ...(sessions ? { sessions } : {}) };
}

/// A bot's own session as the state read lists it, doing `activity`.
const own = (id: string, activity: SessionSummaryView["activity"]): SessionSummaryView => ({
  id,
  access: { visibility: "universe" },
  activity,
  lifecycleStatus: "open",
  lifecycleControlled: true,
  createdAtMs: 0,
  updatedAtMs: 0,
  retention: { rootSessionId: id },
});

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
  access: { visibility: "universe" },
  activity: "idle",
  lifecycleStatus: "open",
  lifecycleControlled: true,
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
    limits: { maxDepth: 1, maxConcurrent: 1, runTimeoutMs: 60_000 },
  },
});

describe("botConversations", () => {
  it("is just Main for a bot with one session", () => {
    expect(botConversations(state({})).map((conversation) => [conversation.kind, conversation.title])).toEqual([["main", "Main"]]);
  });

  it("lists Main, then each thread with the sub-agents under it, then Main's sub-agents", () => {
    const current = state(
      {
        sessions: [
          { sessionId: "bot:v1:triage", label: "main", kind: "main", busy: false, generation: 1 },
          thread("t-old", "PR-1", 1),
          thread("t-new", "PR-2", 2),
        ],
        recentDeliveries: [
          { deliveryId: "d-1", sessionId: "bot:v1:triage", seqs: [3], outcome: "handled", summary: "Triaged #3", finishedAtMs: 5 },
        ],
      },
      [own("t-new", "working"), subagent("s-thread", "Reviewer", "t-new", 4), subagent("s-main", "Explorer", "bot:v1:triage", 3)],
    );
    const conversations = botConversations(current);
    expect(conversations.map((conversation) => conversation.id)).toEqual(["bot:v1:triage", "t-new", "s-thread", "t-old", "s-main"]);
    // Main is worded as the bot's own row words its latest work.
    expect(conversations[0]).toMatchObject({ kind: "main", title: "Main", line: "#3 handled · Triaged #3" });
    expect(conversations[1]).toMatchObject({ kind: "thread", title: "PR-2", line: "Working", tone: "live" });
    // A sub-agent says who delegated to it.
    expect(conversations[2]).toMatchObject({ kind: "subagent", title: "Reviewer", parent: "PR-2" });
    expect(conversations[4]).toMatchObject({ kind: "subagent", title: "Explorer", parent: "Main" });
  });

  it("has none before the controller reports", () => {
    expect(botConversations(undefined)).toEqual([]);
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
    expect(environmentSummary(undefined)).toBe("No default environment");
    expect(environmentSummary({ features: { environments: { environments: [{ environmentId: "env-1", access: "read", default: true }] } } })).toBe("env-1");
  });
});
