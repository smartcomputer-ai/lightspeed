import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { RoomBuffer, TurnDebouncer, type RoomEventItem } from "../src/batcher.js";

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("TurnDebouncer", () => {
  it("batches a burst into one flush after the quiet window", () => {
    const flushes: string[][] = [];
    const debouncer = new TurnDebouncer<string>({
      debounceMs: 500,
      maxBatch: 10,
      maxWaitMs: 2_500,
      onFlush: (_key, batch) => flushes.push(batch),
    });

    debouncer.push("chat", "one");
    vi.advanceTimersByTime(200);
    debouncer.push("chat", "two");
    vi.advanceTimersByTime(200);
    debouncer.push("chat", "three");
    expect(flushes).toHaveLength(0);

    vi.advanceTimersByTime(500);
    expect(flushes).toEqual([["one", "two", "three"]]);
  });

  it("flushes immediately at maxBatch", () => {
    const flushes: string[][] = [];
    const debouncer = new TurnDebouncer<string>({
      debounceMs: 500,
      maxBatch: 2,
      maxWaitMs: 2_500,
      onFlush: (_key, batch) => flushes.push(batch),
    });

    debouncer.push("chat", "one");
    debouncer.push("chat", "two");
    expect(flushes).toEqual([["one", "two"]]);
  });

  it("caps total wait at maxWaitMs under sustained traffic", () => {
    const flushes: string[][] = [];
    const debouncer = new TurnDebouncer<string>({
      debounceMs: 500,
      maxBatch: 100,
      maxWaitMs: 1_000,
      onFlush: (_key, batch) => flushes.push(batch),
    });

    debouncer.push("chat", "m0");
    for (let index = 1; index <= 4; index += 1) {
      vi.advanceTimersByTime(400);
      debouncer.push("chat", `m${index}`);
    }
    // 4 * 400ms elapsed > maxWaitMs; the rearmed timer fires at the cap.
    vi.advanceTimersByTime(400);
    expect(flushes).toHaveLength(1);
  });

  it("keeps conversations independent", () => {
    const flushes: Array<[string, string[]]> = [];
    const debouncer = new TurnDebouncer<string>({
      debounceMs: 500,
      maxBatch: 10,
      maxWaitMs: 2_500,
      onFlush: (key, batch) => flushes.push([key, batch]),
    });

    debouncer.push("a", "one");
    debouncer.push("b", "two");
    vi.advanceTimersByTime(500);
    expect(flushes).toEqual([
      ["a", ["one"]],
      ["b", ["two"]],
    ]);
  });
});

describe("RoomBuffer", () => {
  function event(id: number): RoomEventItem {
    return { key: `key-${id}`, text: `message ${id}` };
  }

  it("flushes on the timer", async () => {
    const flushes: RoomEventItem[][] = [];
    const buffer = new RoomBuffer({
      flushMs: 30_000,
      flushMax: 20,
      budget: 50,
      onFlush: async (_key, events) => {
        flushes.push(events);
      },
    });

    buffer.push("chat", event(1));
    buffer.push("chat", event(2));
    expect(flushes).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(30_000);
    expect(flushes).toEqual([[event(1), event(2)]]);
    expect(buffer.bufferedCount("chat")).toBe(0);
  });

  it("flushes when reaching flushMax", async () => {
    const flushes: RoomEventItem[][] = [];
    const buffer = new RoomBuffer({
      flushMs: 30_000,
      flushMax: 3,
      budget: 50,
      onFlush: async (_key, events) => {
        flushes.push(events);
      },
    });

    buffer.push("chat", event(1));
    buffer.push("chat", event(2));
    buffer.push("chat", event(3));
    await vi.runAllTimersAsync();
    expect(flushes).toEqual([[event(1), event(2), event(3)]]);
  });

  it("drops the oldest events beyond the budget and reports the count", async () => {
    let reported: { events: RoomEventItem[]; dropped: number } | null = null;
    const buffer = new RoomBuffer({
      flushMs: 30_000,
      flushMax: 100,
      budget: 3,
      onFlush: async (_key, events, dropped) => {
        reported = { events, dropped };
      },
    });

    for (let index = 1; index <= 5; index += 1) {
      buffer.push("chat", event(index));
    }
    await buffer.drain("chat");
    expect(reported).toEqual({
      events: [event(3), event(4), event(5)],
      dropped: 2,
    });
  });

  it("re-buffers events when a flush fails", async () => {
    let attempts = 0;
    const flushes: RoomEventItem[][] = [];
    const buffer = new RoomBuffer({
      flushMs: 30_000,
      flushMax: 100,
      budget: 50,
      log: () => undefined,
      onFlush: async (_key, events) => {
        attempts += 1;
        if (attempts === 1) {
          throw new Error("gateway offline");
        }
        flushes.push(events);
      },
    });

    buffer.push("chat", event(1));
    await expect(buffer.drain("chat")).rejects.toThrow("gateway offline");
    expect(buffer.bufferedCount("chat")).toBe(1);
    await buffer.drain("chat");
    expect(flushes).toEqual([[event(1)]]);
  });
});
