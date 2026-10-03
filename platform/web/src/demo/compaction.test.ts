import { describe, expect, it } from "vitest";
import type { ContextEntryView, SessionEventView } from "@lightspeed-ai/sdk";
import { applyEvents, emptyTranscript } from "@/lib/sessions/transcript";
import { createDemoStore } from "./fixtures";
import { SOFTWARE_FACTORY_UNIVERSE_ID } from "./fixtures/software-factory";
import { PERSONAL_ASSISTANT_UNIVERSE_ID } from "./fixtures/personal-assistant";
import { createDemoRouter } from "./router";

function appliedEntries(events: SessionEventView[]): ContextEntryView[] {
  return events.flatMap((event) => event.kind.type === "contextEntriesApplied" ? event.kind.entries : []);
}

describe("demo compaction conversations", () => {
  it("compacts an implementation run between tool batches while retaining two settled exchanges", () => {
    const store = createDemoStore();
    const session = [...store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)!.sessions.values()]
      .find((session) => session.view.id.includes("implementer:k-lin-1421-a"))!;
    expect(session).toBeDefined();
    expect(session.view.activeContext?.compaction).toMatchObject({
      requestedMode: "providerStandalone", effectiveStrategy: "nativePreferred",
      inputLimitTokens: 10_000, compactThresholdTokens: 8_000, thresholdSource: "inputCapacity",
      pending: false, queued: false,
    });

    const requestedIndex = session.events.findIndex((event) => event.kind.type === "contextCompactionRequested");
    expect(requestedIndex).toBeGreaterThan(0);
    const requested = session.events[requestedIndex]!;
    const runId = requested.joins?.runId;
    expect(requested.kind).toMatchObject({ trigger: "highWatermark" });
    expect(runId).toBeTruthy();
    const older = appliedEntries(session.events.slice(0, requestedIndex));
    const batches = session.events.slice(0, requestedIndex).filter((event) => event.kind.type === "toolBatchCompleted");
    expect(batches).toHaveLength(3);
    const retainedTurns = new Set(batches.slice(-2).map((event) => event.joins?.turnId));
    const retained = older.filter((entry) => entry.source && "turnId" in entry.source && retainedTurns.has(entry.source.turnId));
    expect(retained.filter((entry) => entry.kind.type === "toolCall").length).toBeGreaterThan(0);
    expect(retained.filter((entry) => entry.kind.type === "toolResult").length).toBeGreaterThan(0);

    const operation = session.events.slice(requestedIndex, requestedIndex + 4);
    expect(operation.map((event) => event.kind.type)).toEqual([
      "contextCompactionRequested", "contextEntriesRemoved", "contextEntriesApplied", "contextCompactionFinished",
    ]);
    expect(operation.every((event) => event.joins?.runId === runId)).toBe(true);
    const removed = operation[1]!.kind;
    expect(removed.type).toBe("contextEntriesRemoved");
    if (removed.type !== "contextEntriesRemoved") throw new Error("Missing compacted prefix removal");
    expect(removed.entryIds.length).toBeGreaterThan(0);
    expect(retained.every((entry) => !removed.entryIds.includes(entry.id))).toBe(true);
    for (const entry of retained) expect(session.activeContext.entries.find((active) => active.id === entry.id)).toEqual(entry);
    expect(removed.entryIds.every((id) => !session.activeContext.entries.some((entry) => entry.id === id))).toBe(true);
    const replacement = appliedEntries(operation)[0]!;
    expect(replacement).toMatchObject({ kind: { type: "providerOpaque" }, source: { type: "runtime", label: "standalone_compaction_prefix" } });
    expect(store.readText(replacement.content!.contentRef)).toContain("#482");

    const following = session.events.slice(requestedIndex + 4).filter((event) => event.joins?.runId === runId);
    expect(following.some((event) => event.kind.type === "toolBatchCompleted")).toBe(true);
    expect(following.at(-1)?.kind.type).toBe("runCompleted");
    expect(session.runs.get(runId!)?.outputText).toContain("#491");
    const transcript = applyEvents(emptyTranscript(), session.events);
    expect(transcript.entries.filter((entry) => entry.kind === "marker" && entry.text === "context compacted")).toHaveLength(1);
    expect(transcript.entries.some((entry) => entry.kind === "message" && entry.key === replacement.id)).toBe(false);
    const summary = transcript.entries.find((entry) => entry.kind === "run-summary" && entry.runId === runId);
    const generations = session.events.filter((event) => event.joins?.runId === runId && event.kind.type === "turnGenerationCompleted");
    expect(summary).toMatchObject({ status: "completed", usageComplete: true, usage: { modelCalls: generations.length + 1 } });
    const inputTokens = generations.reduce((sum, event) => sum + (event.kind.type === "turnGenerationCompleted" ? event.kind.usage?.inputTokens ?? 0 : 0), 0);
    const finished = operation[3]!.kind;
    expect(finished.type).toBe("contextCompactionFinished");
    if (finished.type !== "contextCompactionFinished") throw new Error("Missing compaction completion");
    expect(summary).toMatchObject({ usage: { inputTokens: inputTokens + (finished.usage?.inputTokens ?? 0) } });
  });

  it("shows native triggered compaction in Ada's thread and continues with her existing commitments", () => {
    const store = createDemoStore();
    const session = store.universe(PERSONAL_ASSISTANT_UNIVERSE_ID)!.sessions.get("bot:v1:assistant:k-ada")!;
    expect(session.view.activeContext?.compaction).toMatchObject({
      requestedMode: "providerTriggered", effectiveMode: "providerTriggered", effectiveStrategy: "providerTriggered",
      inputLimitTokens: 128_000, compactThresholdTokens: 50_000, thresholdSource: "override",
    });
    expect(session.events.some((event) => event.kind.type === "contextCompactionRequested" || event.kind.type === "contextCompactionFinished")).toBe(false);
    const replacement = appliedEntries(session.events).find((entry) => entry.kind.type === "providerOpaque"
      && entry.content?.providerKind === "anthropic.messages.compaction")!;
    expect(replacement.source?.type).toBe("assistantOutput");
    const payload = store.readText(replacement.content!.contentRef)!;
    expect(payload).toContain("cohort retention cut and three cleared references");
    const removed = session.events.find((event) => event.kind.type === "contextEntriesRemoved" && event.kind.reason === "providerCompacted")!;
    expect(removed.kind.type).toBe("contextEntriesRemoved");
    if (removed.kind.type !== "contextEntriesRemoved") throw new Error("Missing native compaction pruning");
    expect(removed.kind.entryIds.length).toBeGreaterThan(0);
    expect(session.activeContext.entries.some((entry) => entry.id === replacement.id)).toBe(true);
    expect(removed.kind.entryIds.every((id) => !session.activeContext.entries.some((entry) => entry.id === id))).toBe(true);
    const transcript = applyEvents(emptyTranscript(), session.events);
    expect(transcript.entries.filter((entry) => entry.kind === "marker" && entry.text === "context compacted")).toHaveLength(1);
    expect(transcript.entries.some((entry) => entry.kind === "message" && entry.text.includes("MRR") && entry.text.includes("412"))).toBe(true);
    expect(transcript.entries.some((entry) => entry.kind === "message" && entry.role === "user" && entry.text.includes("send it"))).toBe(true);
    expect(transcript.entries.some((entry) => entry.kind === "message" && entry.role === "assistant" && entry.text.includes("cut attached from the data room") && entry.text.includes("three cleared references"))).toBe(true);
    expect(transcript.entries.some((entry) => entry.kind === "message" && entry.text.includes("demo-compaction-signature"))).toBe(false);
  });

  it("refreshes compaction settings when a demo user edits the seeded policy", async () => {
    const store = createDemoStore();
    const session = store.universe(PERSONAL_ASSISTANT_UNIVERSE_ID)!.sessions.get("bot:v1:assistant:k-ada")!;
    const app = createDemoRouter(store);
    const path = `http://demo.local/api/v1/universes/${PERSONAL_ASSISTANT_UNIVERSE_ID}/sessions/${session.view.id}/config`;
    const response = await app.fetch(new Request(path, {
      method: "PUT", headers: { "content-type": "application/json" },
      body: JSON.stringify({ expectedConfigRevision: session.view.configRevision,
        config: { ...session.view.config, context: { inputLimitTokens: 128_000, compaction: { mode: "disabled" } } } }),
    }));
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({ activeContext: { compaction: {
      requestedMode: "disabled", effectiveMode: "disabled", effectiveStrategy: "disabled",
      compactThresholdTokens: null, thresholdSource: "disabled", observedTokens: null,
    } } });
  });
});
