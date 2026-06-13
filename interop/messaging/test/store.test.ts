import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
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
    const created = await store.getOrCreateBinding("conv-1", init);
    expect(created.sessionId).toBe("bridge_abc");
    expect(created.generation).toBe(0);

    await store.updateBinding("conv-1", { activation: "always" });

    const reloaded = new JsonBridgeStore(filePath);
    const binding = await reloaded.getBinding("conv-1");
    expect(binding?.activation).toBe("always");
    expect(binding?.chatId).toBe("chat-1");
  });

  it("migrates legacy conversation state into the binding", async () => {
    await writeFile(
      filePath,
      JSON.stringify({
        conversations: {
          "conv-1": { sessionId: "legacy_session", cursor: { seq: 7 }, updatedAtMs: 1 },
        },
        messages: {},
      }),
    );

    const store = new JsonBridgeStore(filePath);
    const binding = await store.getOrCreateBinding("conv-1", init);
    expect(binding.sessionId).toBe("legacy_session");
    expect(binding.cursor).toEqual({ seq: 7 });

    const raw = JSON.parse(await readFile(filePath, "utf8")) as {
      conversations: Record<string, unknown>;
    };
    expect(raw.conversations["conv-1"]).toBeUndefined();
  });

  it("returns the existing binding without overwriting customizations", async () => {
    const store = new JsonBridgeStore(filePath);
    await store.getOrCreateBinding("conv-1", init);
    await store.updateBinding("conv-1", { activation: "silent", generation: 2 });

    const again = await store.getOrCreateBinding("conv-1", { ...init, activation: "always" });
    expect(again.activation).toBe("silent");
    expect(again.generation).toBe(2);
  });
});
