import { describe, expect, it, vi } from "vitest";
import { runTypingLoop } from "../src/providers/typing.js";

describe("provider typing activity", () => {
  it("pulses, heartbeats, and clears presence when Temporal cancels it", async () => {
    let cancel!: (reason: Error) => void;
    const cancelled = new Promise<never>((_resolve, reject) => {
      cancel = reject;
    });
    const pulse = vi.fn(async () => undefined);
    const clear = vi.fn(async () => undefined);
    const heartbeat = vi.fn();
    const running = runTypingLoop(
      { route: { provider: "whatsapp", accountId: "primary", chatId: "family@g.us" } },
      {
        provider: "whatsapp",
        accountId: "primary",
        intervalMs: 60_000,
        pulse,
        clear,
      },
      { cancelled, heartbeat },
    );
    await vi.waitFor(() => expect(pulse).toHaveBeenCalledOnce());
    cancel(new Error("cancelled"));
    await expect(running).rejects.toThrow("cancelled");
    expect(heartbeat).toHaveBeenCalledOnce();
    expect(clear).toHaveBeenCalledOnce();
  });

  it("rejects a task delivered to the wrong account before provider calls", async () => {
    const pulse = vi.fn(async () => undefined);
    await expect(
      runTypingLoop(
        { route: { provider: "telegram", accountId: "other", chatId: "123" } },
        {
          provider: "telegram",
          accountId: "primary",
          intervalMs: 4_000,
          pulse,
          clear: async () => undefined,
        },
        { cancelled: new Promise<never>(() => undefined), heartbeat: () => undefined },
      ),
    ).rejects.toThrow(/wrong provider worker/);
    expect(pulse).not.toHaveBeenCalled();
  });
});
