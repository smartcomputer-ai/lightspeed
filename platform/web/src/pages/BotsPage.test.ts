import { describe, expect, it } from "vitest";
import type { BotEventView, BotListItem, BotStateView, SessionSummary } from "@/api";
import { rosterLine } from "./BotsPage";
import { botSubRows, subRowKindMark, threadLine } from "@/components/bot/conversations";
import { SessionSubThreadIcon } from "@/icons";
import { BOT_TEMPLATES } from "@/components/bot/templates";
import { capabilitySummary } from "@/components/bot/setup-summary";
import { botIdFrom, botOwnedProfileDocument, templateHighlights, uniqueTriggerName } from "./BotCreatePage";

function event(partial: Partial<BotEventView>): BotEventView {
  return {
    seq: 1,
    eventId: "evt-1",
    documentRef: "blob:sha256:0",
    kind: "x",
    summary: "",
    occurredAtMs: Date.parse("2026-08-27T09:00:00Z"),
    receivedAtMs: Date.parse("2026-08-27T09:00:00Z"),
    ...partial,
  };
}

function bot(partial: Partial<BotListItem>): BotListItem {
  return {
    access: { visibility: "universe" },
    activity: "idle",
    botId: "triage",
    displayName: "Triage",
    profileId: "triage",
    revision: 1,
    eventSeq: 0,
    selfConfig: true,
    emit: false,
    enabled: true,
    createdAtMs: Date.parse("2026-08-27T00:00:00Z"),
    updatedAtMs: Date.parse("2026-08-27T00:00:00Z"),
    triggerCount: 0,
    pendingCount: 0,
    lastEvent: null,
    ...partial,
  };
}

describe("rosterLine", () => {
  it("shows a run someone started by chatting as work, and an approval as waiting", () => {
    expect(rosterLine(bot({ activity: "working" }))).toEqual({ text: "Working", tone: "live" });
    expect(rosterLine(bot({ activity: "waiting", pendingCount: 2 }))).toEqual({ text: "Waiting for approval", tone: "waiting" });
  });
  it("says what the bot is doing from the event log alone", () => {
    expect(rosterLine(bot({}))).toEqual({ text: "Waiting for its first event", tone: "idle" });
    expect(
      rosterLine(
        bot({
          pendingCount: 2,
          lastEvent: event({ seq: 48, kind: "github.pull_request", triggerId: "github-prs" }),
        }),
      ),
    ).toEqual({ text: "Working on #48 · github.pull_request", tone: "live" });
    expect(
      rosterLine(
        bot({
          lastEvent: event({
            seq: 47,
            kind: "schedule.fire",
            triggerId: "schedule",
            outcome: "handled",
            outcomeDetail: "Digest sent",
            resolvedAtMs: Date.parse("2026-08-27T09:01:00Z"),
          }),
        }),
      ),
    ).toEqual({ text: "#47 handled · Digest sent", tone: "idle" });
  });
  it("puts lifecycle before activity", () => {
    expect(rosterLine(bot({ enabled: false, pendingCount: 3 }))).toEqual({ text: "Paused · 3 waiting", tone: "paused" });
    expect(rosterLine(bot({ closedAtMs: Date.parse("2026-08-27T00:00:00Z"), pendingCount: 3 }))).toEqual({
      text: "Closed",
      tone: "closed",
    });
  });
  it("flags a failed last outcome", () => {
    expect(
      rosterLine(
        bot({
          lastEvent: event({
            seq: 42,
            outcome: "runFailed",
            outcomeDetail: "environment suspended",
            resolvedAtMs: Date.parse("2026-08-27T09:01:00Z"),
          }),
        }),
      ),
    ).toEqual({ text: "#42 run failed · environment suspended", tone: "attention" });
  });
});

describe("wizard helpers", () => {
  it("derives an id from the name", () => {
    expect(botIdFrom("Release Shepherd")).toBe("release-shepherd");
    expect(botIdFrom("  Ünïcode! bot ")).toBe("unicode-bot");
  });
  it("keeps wake-up names unique", () => {
    expect(uniqueTriggerName("schedule", [])).toBe("schedule");
    expect(uniqueTriggerName("schedule", ["schedule", "schedule-2"])).toBe("schedule-3");
  });
  it("describes what a template comes with", () => {
    const reviewer = BOT_TEMPLATES.find((template) => template.id === "pr-reviewer")!;
    expect(templateHighlights(reviewer)).toEqual(["GitHub events", "Weekdays at 09:00", "Web"]);
    expect(templateHighlights(BOT_TEMPLATES.find((template) => template.id === "blank")!)).toEqual([]);
    // Every template's suggested name derives to a valid, distinct id.
    const ids = BOT_TEMPLATES.flatMap((template) => (template.suggestedName ? [botIdFrom(template.suggestedName)] : []));
    expect(new Set(ids).size).toBe(ids.length);
    for (const id of ids) expect(id).toMatch(/^[a-z0-9][a-z0-9-]*$/);
  });
  it("summarises capabilities in words", () => {
    expect(
      capabilitySummary({
        model: { model: "claude-opus-5" },
        features: { web: {}, mcp: { servers: [{ serverId: "a" }, { serverId: "b" }] }, subagents: { agents: [{ profileId: "r" }] } },
      }),
    ).toEqual(["claude-opus-5", "Web", "MCP servers (2)", "Sub-agents (1)"]);
  });
  it("builds the complete profile owned by a new bot", () => {
    expect(botOwnedProfileDocument({
      profileId: "triage",
      displayName: "Triage",
      config: { features: { environments: { environments: [{ environmentId: "ops-box", access: "exec", default: true }] } } },
      baseInstructions: "Always cite the incident.",
      metadata: { team: "ops" },
    })).toEqual({
      profileId: "triage",
      displayName: "Triage",
      description: "Setup of bot triage",
      config: { features: { environments: { environments: [{ environmentId: "ops-box", access: "exec", default: true }] } } },
      instructions: { type: "text", text: "Always cite the incident." },
      metadata: { team: "ops" },
    });
  });
});

describe("botSubRows", () => {
  const summary = (id: string, overrides: Partial<SessionSummary> = {}): SessionSummary => ({
    id, lifecycleStatus: "open", activity: "idle", createdAtMs: 1, updatedAtMs: 1, lifecycleControlled: true,
    retention: { rootSessionId: id }, access: { visibility: "universe" }, ...overrides,
  });
  const subagent = (id: string, parent: string, overrides: Partial<SessionSummary> = {}) => summary(id, {
    displayName: `Sub ${id}`,
    origin: {
      kind: "subagent", parentSessionId: parent, parentRunId: "run", rootSessionId: "bot:v1:triage", depth: 1,
      invocationId: "inv", agent: { profileId: "reviewer", revision: 1 }, limits: { maxDepth: 2, maxConcurrent: 1, runTimeoutMs: 1 },
    },
    ...overrides,
  });
  const state = {
    controller: {
      mainSessionId: "bot:v1:triage",
      sessions: [
        { sessionId: "bot:v1:triage", label: "main", kind: "main", busy: false, generation: 1 },
        { sessionId: "t-old", label: "PR-1", kind: "perKey", busy: false, generation: 1, lastActiveAtMs: 1 },
        { sessionId: "t-new", label: "PR-2", kind: "perKey", busy: false, generation: 1, lastActiveAtMs: 2 },
      ],
    },
    sessions: [
      summary("t-new", { activity: "working" }),
      subagent("s-main", "bot:v1:triage"),
      subagent("s-thread", "t-new", { lifecycleStatus: "closed" }),
      subagent("s-nested", "s-thread"),
    ],
  } as unknown as BotStateView;

  it("lists the threads, newest first, and hangs each sub-agent off who delegated to it", () => {
    const rows = botSubRows(state);
    // Main's sub-agent hangs off the bot itself, after the threads.
    expect(rows.map((row) => row.id)).toEqual(["t-new", "t-old", "s-main"]);
    const [newest, old, main] = rows;
    expect(newest).toMatchObject({ title: "PR-2", hint: "Thread: PR-2", kind: { thread: true, label: "Thread" }, line: "Working", tone: "live", atMs: 2 });
    // Each sub-row's kind is a mark: the thread icon, or the sub-agent icon.
    expect(subRowKindMark(newest!)).toMatchObject({ label: "Thread", icon: SessionSubThreadIcon });
    expect(subRowKindMark(main!)).toMatchObject({ label: "Sub-agent", tooltip: "Sub-agent, running reviewer" });
    expect(old!.children).toEqual([]);
    expect(main).toMatchObject({ title: "Sub s-main", line: "reviewer · Idle", tone: "idle", closed: false });
    expect(newest!.children.map((row) => row.id)).toEqual(["s-thread"]);
    expect(newest!.children[0]).toMatchObject({ closed: true, line: "reviewer · Closed", tone: "closed" });
    expect(newest!.children[0]!.children.map((row) => row.id)).toEqual(["s-nested"]);
  });

  it("has none before the controller reports", () => {
    expect(botSubRows(undefined)).toEqual([]);
    expect(botSubRows({ sessions: [] } as unknown as BotStateView)).toEqual([]);
  });
});

describe("threadLine", () => {
  const base = { activity: "idle" as const, active: undefined, recent: undefined, preview: undefined, kind: "Thread" };

  it("says what waits on a thread, then what it is working on", () => {
    expect(threadLine({ ...base, activity: "waiting", active: { seqs: [8] } })).toEqual({ line: "Waiting for approval", tone: "waiting", failed: false });
    expect(threadLine({ ...base, active: { seqs: [7, 9] } })).toEqual({ line: "Working on #9", tone: "live", failed: false });
    expect(threadLine({ ...base, activity: "working" })).toEqual({ line: "Working", tone: "live", failed: false });
  });

  it("says how its latest delivery ended, as the bot's row does", () => {
    const recent = { seqs: [7], outcome: "handled" as const, summary: "PR #493 opened" };
    expect(threadLine({ ...base, recent, preview: "older words" }).line).toBe("#7 handled · PR #493 opened");
    // Without the bot's summary, its latest message; without that, its kind.
    expect(threadLine({ ...base, recent: { ...recent, summary: null }, preview: "Latest message" }).line).toBe("#7 handled · Latest message");
    expect(threadLine({ ...base, recent: { ...recent, summary: " " } }).line).toBe("#7 handled · Thread");
    expect(threadLine({ ...base, recent: { ...recent, outcome: "runFailed" } })).toMatchObject({ line: "#7 run failed · PR #493 opened", tone: "attention", failed: true });
  });

  it("falls back to its latest message, then its kind", () => {
    expect(threadLine({ ...base, preview: "Shipped the fix" }).line).toBe("Shipped the fix");
    expect(threadLine(base)).toEqual({ line: "Thread", tone: "idle", failed: false });
  });
});
