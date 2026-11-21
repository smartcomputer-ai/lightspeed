import { expect, it, vi } from "vitest";
import type { BotEventView } from "@lightspeed-ai/sdk";
import type { SessionSummary } from "@/api";
import { metaItemsFor } from "@/components/meta-toolbar";
import { activityTip, botMetaItems, creatorLabel } from "./bot-meta";

const status = { label: "Idle", tone: "idle" as const };

function subagent(id: string, displayName: string): SessionSummary {
  return {
    id, displayName, lifecycleStatus: "open", activity: "idle", createdAtMs: 1, updatedAtMs: 1,
    lifecycleControlled: false, retention: { rootSessionId: "bot:v1:triage" }, access: { visibility: "universe" },
    origin: {
      kind: "subagent", parentSessionId: "bot:v1:triage", parentRunId: "run", rootSessionId: "bot:v1:triage", depth: 1,
      invocationId: "inv", agent: { profileId: "reviewer", revision: 1 }, limits: { maxDepth: 1, maxConcurrent: 1, runTimeoutMs: 1 },
    },
  };
}

const event = (overrides: Partial<BotEventView> = {}): BotEventView => ({
  seq: 14, kind: "webhook", eventId: "e-14", documentRef: "doc", summary: "Deploy of v2.14.1 finished", outcome: "handled",
  occurredAtMs: Date.now() - 5 * 60_000, receivedAtMs: Date.now() - 5 * 60_000, ...overrides,
} as BotEventView);

it("counts waiting events in a list row, and in the activity item on the bot's page", () => {
  const row = botMetaItems({ status, pending: 3, closed: false });
  expect(metaItemsFor(row, "list").map((item) => item.key)).toEqual(["pending"]);

  const open = vi.fn();
  const page = botMetaItems({ status, pending: 3, closed: false, activity: { open } });
  expect(metaItemsFor(page, "list").map((item) => item.key)).toEqual(["pending"]);
  const header = metaItemsFor(page, "header");
  expect(header.map((item) => item.key)).toEqual(["status", "activity"]);
  expect(header[1]).toMatchObject({ text: "Activity · 3 waiting", value: 3, tone: "info" });
  // It opens the activity dialog instead of navigating.
  expect(header[1]!.href).toBeUndefined();
  header[1]!.onSelect!();
  expect(open).toHaveBeenCalledOnce();
});

it("offers the activity on the bot's page even with nothing waiting", () => {
  const page = botMetaItems({ status, pending: 0, closed: false, activity: { open: () => {} } });
  expect(page.find((item) => item.key === "activity")).toMatchObject({ text: "Activity" });
  expect(page.find((item) => item.key === "activity")?.tone).toBeUndefined();
});

it("names the latest event in the activity tooltip, then what waits", () => {
  expect(activityTip(event(), null)).toEqual(["Latest: #14 webhook · handled · 5m ago", "Deploy of v2.14.1 finished"]);
  expect(activityTip(event({ outcome: null, summary: "x".repeat(200) }), "3 events waiting to be handled")).toEqual([
    "Latest: #14 webhook · pending · 5m ago",
    `${"x".repeat(119)}…`,
    "3 events waiting to be handled",
  ]);
  expect(activityTip(event({ outcome: "runFailed" }), null)[0]).toContain("run failed");
  // What came of it, as the activity list says it, wins over the raw summary.
  expect(activityTip(event({ outcomeDetail: "PR #493 opened" }), null)[1]).toBe("PR #493 opened");
  expect(activityTip(null, null)).toEqual(["Activity: no events yet"]);
});

it("lists the bot's sub-agents on its page, the first by name in the tooltip", () => {
  const href = (id: string) => `/chat/${id}`;
  const none = botMetaItems({ status, pending: 0, closed: false, subAgents: { sessions: [], href } });
  expect(none.find((item) => item.key === "subagents")).toBeUndefined();

  const two = botMetaItems({ status, pending: 0, closed: false, subAgents: { sessions: [subagent("a", "Review PR-4"), subagent("b", "Run tests")], href } });
  const item = two.find((candidate) => candidate.key === "subagents")!;
  expect(item).toMatchObject({ value: 2, text: "2 sub-agents", tooltip: "Sub-agents: Review PR-4 and 1 more", places: ["header", "strip"] });
  expect(item.details).toBeDefined();
});

it("marks who sees the bot, and says who created it", () => {
  const items = botMetaItems({
    status, pending: 0, closed: false, viewerId: "me",
    access: { visibility: "universe", createdBy: { kind: "actor", id: "me" } },
  });
  const sharing = items.find((item) => item.key === "sharing")!;
  expect(sharing).toMatchObject({ label: "Shared", text: "Shared" });
  // Shown in a list row and on the bot's page alike.
  expect(sharing.places).toBeUndefined();
  expect(creatorLabel({ kind: "actor", id: "me" }, "me")).toBe("you");
  expect(creatorLabel({ kind: "actor", id: "someone" }, "me")).toBe("a member");
  expect(creatorLabel({ kind: "token", name: "operator" }, "me")).toBe("operator");
  expect(creatorLabel({ kind: "internal", cause: "seed", component: "demo" }, "me")).toBe("the system");
  // Without access, no mark.
  expect(botMetaItems({ status, pending: 0, closed: false }).some((item) => item.key === "sharing")).toBe(false);
});
