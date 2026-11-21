import { describe, expect, it } from "vitest";
import type { BotTriggerView } from "@lightspeed-ai/sdk";
import { deliverySentence, deliveryShapeOf, describeCron, triggerSummary } from "./trigger-summary";

const base = {
  botId: "triage",
  triggerId: "t",
  revision: 1,
  createdAtMs: 0,
  updatedAtMs: 0,
};

function trigger(fields: Record<string, unknown>): BotTriggerView {
  return { ...base, ...fields } as unknown as BotTriggerView;
}

describe("describeCron", () => {
  it("speaks the builder's shapes", () => {
    expect(describeCron("0 9 * * 1-5", "Europe/Berlin")).toBe("Weekdays at 09:00 Europe/Berlin");
    expect(describeCron("30 8 * * *", "UTC")).toBe("Every day at 08:30 UTC");
    expect(describeCron("*/15 * * * *")).toBe("Every 15 minutes");
    expect(describeCron("0 10 * * 1")).toBe("Mondays at 10:00");
  });
  it("falls back to the expression it cannot express", () => {
    expect(describeCron("5 4 1,15 * *", "UTC")).toBe("5 4 1,15 * * UTC");
  });
});

describe("triggerSummary", () => {
  it("names the source and how it is verified or scoped", () => {
    expect(
      triggerSummary(trigger({ kind: "webhook", preset: "github", verification: { scheme: "token" } })),
    ).toBe("GitHub webhook · URL token");
    expect(
      triggerSummary(
        trigger({
          kind: "poll",
          source: { kind: "http", url: "https://api.example.com/items" },
          intervalMs: 300_000,
          items: null,
          cursor: { kind: "idSet", id: "id" },
        }),
      ),
    ).toBe("Checks api.example.com every 5 min");
    expect(triggerSummary(trigger({ kind: "bot", from: ["release-shepherd"] }))).toBe(
      "Messages from release-shepherd",
    );
  });
});

describe("deliverySentence", () => {
  it("reads routing, batching, busy handling, and idle close as one line", () => {
    const shape = deliveryShapeOf(
      trigger({
        kind: "webhook",
        verification: { scheme: "token" },
        route: { policy: "perKey", key: "data.pull_request.number" },
        coalesce: { debounceMs: 30_000, maxWaitMs: 120_000, maxCount: 20 },
        deliver: { whenBusy: "queue" },
        sessionCloseAfterMs: 7 * 24 * 3_600_000,
      }),
    );
    expect(deliverySentence(shape)).toBe(
      "one thread per data.pull_request.number · batches for up to 120s · queues when busy · threads close after 168h idle",
    );
  });
  it("keeps chat vocabulary", () => {
    expect(
      deliverySentence(
        {
          routePolicy: "perKey",
          routeKey: "",
          filter: "",
          whenBusy: "steer",
          debounceSeconds: "0.4",
          maxWaitSeconds: "1.5",
          closeMode: "forever",
          closeHours: "",
        },
      ),
    ).toBe("one thread per key · batches for up to 1.5s · steers a busy run · threads kept");
  });
});
