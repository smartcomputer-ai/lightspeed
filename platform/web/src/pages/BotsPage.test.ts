import { describe, expect, it } from "vitest";
import type { BotEventView, BotListItem } from "@/api";
import { rosterLine } from "./BotsPage";
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
    access: { root: { kind: "bot", id: "triage" }, owner: "00000000-0000-0000-0000-000000000000", visibility: "universe" },
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
            outcome: "run_failed",
            outcomeDetail: "environment suspended",
            resolvedAtMs: Date.parse("2026-08-27T09:01:00Z"),
          }),
        }),
      ).tone,
    ).toBe("attention");
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
      retention: 604_800_000,
    })).toEqual({
      profileId: "triage",
      displayName: "Triage",
      description: "Setup of bot triage",
      config: { features: { environments: { environments: [{ environmentId: "ops-box", access: "exec", default: true }] } } },
      instructions: { type: "text", text: "Always cite the incident." },
      metadata: { team: "ops" },
      retention: { deleteAfterCloseMs: 604_800_000 },
    });
  });
});
