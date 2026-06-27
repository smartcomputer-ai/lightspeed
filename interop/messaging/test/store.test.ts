import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { JsonBridgeStore } from "../src/store.js";

let dir: string;
let filePath: string;

beforeEach(async () => {
  dir = await mkdtemp(path.join(tmpdir(), "bridge-store-"));
  filePath = path.join(dir, "state.json");
});

afterEach(async () => {
  await rm(dir, { recursive: true, force: true });
});

const init = {
  channel: "telegram",
  accountId: "default",
  chatId: "chat-1",
  sessionId: "bridge_abc",
  activation: "mention" as const,
};

describe("JsonBridgeStore bindings", () => {
  it("creates and persists bindings across store instances", async () => {
    const store = new JsonBridgeStore(filePath);
    const created = await store.getOrCreateBinding("conv-1", { ...init, profileLabel: "support" });
    expect(created.sessionId).toBe("bridge_abc");
    expect(created.profileLabel).toBe("support");

    await store.updateBinding("conv-1", { activation: "always" });

    const reloaded = new JsonBridgeStore(filePath);
    const binding = await reloaded.getBinding("conv-1");
    expect(binding?.activation).toBe("always");
    expect(binding?.chatId).toBe("chat-1");
    expect(binding?.profileLabel).toBe("support");
  });

  it("returns the existing binding without overwriting customizations", async () => {
    const store = new JsonBridgeStore(filePath);
    await store.getOrCreateBinding("conv-1", init);
    await store.updateBinding("conv-1", { activation: "silent" });

    const again = await store.getOrCreateBinding("conv-1", { ...init, activation: "always" });
    expect(again.activation).toBe("silent");
  });

  it("refreshes routing-owned fields on existing bindings", async () => {
    const store = new JsonBridgeStore(filePath);
    await store.getOrCreateBinding("conv-1", {
      ...init,
      sessionId: "bridge_old",
      profileLabel: null,
    });
    await store.updateBinding("conv-1", { activation: "silent" });

    const refreshed = await store.getOrCreateBinding("conv-1", {
      ...init,
      chatId: "chat-2",
      sessionId: "bridge_new",
      profileLabel: "anna",
      activation: "always",
    });

    expect(refreshed.chatId).toBe("chat-2");
    expect(refreshed.sessionId).toBe("bridge_new");
    expect(refreshed.profileLabel).toBe("anna");
    expect(refreshed.activation).toBe("silent");
  });
});

describe("JsonBridgeStore pairings", () => {
  it("creates and persists paired conversations", async () => {
    const store = new JsonBridgeStore(filePath);
    await store.pairConversation("pair-1", {
      channel: "telegram",
      accountId: "default",
      chatId: "chat-1",
      bindingId: "lukas-telegram",
    });

    const reloaded = new JsonBridgeStore(filePath);
    const pairing = await reloaded.getPairing("pair-1");
    expect(pairing?.bindingId).toBe("lukas-telegram");
    expect(pairing?.chatId).toBe("chat-1");
    expect(pairing?.pairedAtMs).toBeGreaterThan(0);
  });
});
