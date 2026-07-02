import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { TurnDebouncer } from "../src/batcher.js";

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
