import type { LightspeedClient } from "@lightspeed-ai/client";

/** Re-lease this long before the broker's expiry. */
export const LEASE_EXPIRY_MARGIN_MS = 30_000;
/** Longest a token without an expiry is held in memory. */
export const LEASE_MAX_AGE_MS = 5 * 60_000;

/** Something that hands out the current provider token and forgets it on demand. */
export interface TokenSource {
  get(): Promise<string>;
  /** Drop the cached token, e.g. after the provider answered 401. */
  invalidate(): void;
}

/**
 * In-memory lease of one retrievable grant (`auth/grants/lease`). The token is
 * never persisted or placed in a workflow payload; it is cached until
 * `expiresAtMs - 30 s` (or five minutes without an expiry) and re-leased on
 * demand after the provider rejects it.
 */
export class GrantLease implements TokenSource {
  private cached: { token: string; validUntilMs: number } | undefined;
  private inflight: Promise<string> | undefined;

  constructor(
    private readonly client: Pick<LightspeedClient, "call">,
    private readonly grantId: string,
    private readonly now: () => number = Date.now,
  ) {
    if (grantId.length === 0) {
      throw new TypeError("grantId must not be empty");
    }
  }

  async get(): Promise<string> {
    const cached = this.cached;
    if (cached !== undefined && this.now() < cached.validUntilMs) {
      return cached.token;
    }
    this.inflight ??= this.lease().finally(() => {
      this.inflight = undefined;
    });
    return this.inflight;
  }

  invalidate(): void {
    this.cached = undefined;
  }

  private async lease(): Promise<string> {
    const leasedAtMs = this.now();
    const response = await this.client.call("auth/grants/lease", { grantId: this.grantId });
    const lease = response.result;
    if (typeof lease.token !== "string" || lease.token.length === 0) {
      throw new TypeError(`auth/grants/lease returned no token for ${this.grantId}`);
    }
    const untilExpiry =
      lease.expiresAtMs === undefined || lease.expiresAtMs === null
        ? Number.POSITIVE_INFINITY
        : lease.expiresAtMs - LEASE_EXPIRY_MARGIN_MS;
    const validUntilMs = Math.min(untilExpiry, leasedAtMs + LEASE_MAX_AGE_MS);
    this.cached = { token: lease.token, validUntilMs };
    return lease.token;
  }
}
