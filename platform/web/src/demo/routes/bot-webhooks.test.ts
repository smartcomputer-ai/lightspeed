import { expect, it, vi } from "vitest";
import { createDemoStore } from "../fixtures";
import { SOFTWARE_FACTORY_UNIVERSE_ID } from "../fixtures/software-factory";
import { createDemoRouter } from "../router";

it("continues admitting webhook events when the Bots browser module is omitted", async ({ onTestFinished }) => {
  vi.useFakeTimers();
  onTestFinished(() => {
    vi.clearAllTimers();
    vi.useRealTimers();
  });
  const store = createDemoStore();
  const app = createDemoRouter(store, []);
  const universe = store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)!;
  const bot = universe.bots.get("intake")!;
  const hook = bot.triggers.get("linear-webhook")!;
  const eventsBefore = bot.events.length;

  const browser = await app.request(`/api/ui/universes/${universe.universe.id}/modules/bots`);
  expect(browser.status).toBe(404);
  const ingress = await app.request(hook.ingestPath!, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ kind: "issue.created" }),
  });
  expect(ingress.status).toBe(202);
  expect(bot.events).toHaveLength(eventsBefore + 1);
  expect(bot.events.at(-1)).toMatchObject({ triggerId: hook.triggerId, kind: "issue.created" });
});
