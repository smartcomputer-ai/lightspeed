import { describe, expect, it } from "vitest";
import { FixedWindowRateLimiter, parsePositiveInteger } from "../src/runtime/rate-limit.js";

describe("connector ingress rate limiting", () => {
  it("bounds each key and resets on the next fixed window", () => {
    let now = 1_000;
    const limiter = new FixedWindowRateLimiter({
      limit: 2,
      windowMs: 1_000,
      maxKeys: 10,
      now: () => now,
    });
    expect(limiter.allow("chat-a")).toBe(true);
    expect(limiter.allow("chat-a")).toBe(true);
    expect(limiter.allow("chat-a")).toBe(false);
    expect(limiter.allow("chat-b")).toBe(true);
    now = 2_000;
    expect(limiter.allow("chat-a")).toBe(true);
  });

  it("validates environment limits", () => {
    expect(parsePositiveInteger(undefined, 120)).toBe(120);
    expect(parsePositiveInteger("5", 120)).toBe(5);
    expect(() => parsePositiveInteger("0", 120)).toThrow(/positive integer/);
  });
});
