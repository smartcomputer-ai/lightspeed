export interface FixedWindowRateLimiterOptions {
  limit: number;
  windowMs: number;
  maxKeys: number;
  now?: () => number;
}

interface WindowState {
  startedAtMs: number;
  count: number;
}

/** Process-local provider ingress protection; Temporal remains the durable boundary. */
export class FixedWindowRateLimiter {
  private readonly windows = new Map<string, WindowState>();
  private readonly now: () => number;

  constructor(private readonly options: FixedWindowRateLimiterOptions) {
    if (
      !Number.isSafeInteger(options.limit) || options.limit < 1 ||
      !Number.isSafeInteger(options.windowMs) || options.windowMs < 1 ||
      !Number.isSafeInteger(options.maxKeys) || options.maxKeys < 1
    ) {
      throw new TypeError("rate limiter bounds must be positive integers");
    }
    this.now = options.now ?? Date.now;
  }

  allow(key: string): boolean {
    const now = this.now();
    let state = this.windows.get(key);
    if (state === undefined || now - state.startedAtMs >= this.options.windowMs) {
      if (state === undefined && this.windows.size >= this.options.maxKeys) {
        const oldest = this.windows.keys().next().value as string | undefined;
        if (oldest !== undefined) this.windows.delete(oldest);
      }
      state = { startedAtMs: now, count: 0 };
      this.windows.delete(key);
      this.windows.set(key, state);
    }
    if (state.count >= this.options.limit) return false;
    state.count += 1;
    return true;
  }
}

export function parsePositiveInteger(value: string | undefined, fallback: number): number {
  if (value === undefined) return fallback;
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 1) {
    throw new TypeError(`expected a positive integer, got ${value}`);
  }
  return parsed;
}
