import { describe, expect, it, vi } from "vitest";
import { ChannelDeliveryError } from "../src/contracts/delivery.js";
import type { WhatsAppDeliveryApi } from "../src/providers/whatsapp/delivery.js";
import type { WhatsAppPresenceApi } from "../src/providers/whatsapp/presence.js";
import { WhatsAppSocketRegistry } from "../src/providers/whatsapp/socket-registry.js";

describe("WhatsAppSocketRegistry", () => {
  it("always delegates activities to the current reconnect socket", async () => {
    const registry = new WhatsAppSocketRegistry();
    const delegated = registry.deliveryApi();
    const first = {
      sendMessage: vi.fn(async () => ({ key: { id: "1" } })),
      sendPresenceUpdate: vi.fn(async () => undefined),
    } satisfies WhatsAppDeliveryApi & WhatsAppPresenceApi;
    const second = {
      sendMessage: vi.fn(async () => ({ key: { id: "2" } })),
      sendPresenceUpdate: vi.fn(async () => undefined),
    } satisfies WhatsAppDeliveryApi & WhatsAppPresenceApi;

    expect(() => registry.get()).toThrow(ChannelDeliveryError);
    registry.set(first);
    await expect(delegated.sendMessage("chat", { text: "one" })).resolves.toMatchObject({
      key: { id: "1" },
    });
    registry.set(second);
    registry.clear(first);
    expect(registry.connected()).toBe(true);
    await expect(delegated.sendMessage("chat", { text: "two" })).resolves.toMatchObject({
      key: { id: "2" },
    });
    registry.clear(second);
    expect(registry.connected()).toBe(false);
  });
});
