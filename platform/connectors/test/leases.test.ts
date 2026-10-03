import { describe, expect, it, vi } from "vitest";
import type { LightspeedClient } from "@lightspeed-ai/client";
import { GrantLease, LEASE_EXPIRY_MARGIN_MS, LEASE_MAX_AGE_MS } from "../src/core/leases.js";

function client(responses: Array<{ token: string; expiresAtMs?: number | null }>) {
  const call = vi.fn(async (method: string, params: unknown) => {
    expect(method).toBe("auth/grants/lease");
    expect(params).toEqual({ grantId: "grant-1" });
    const next = responses.shift();
    if (next === undefined) throw new Error("no more leases");
    return { result: { grantId: "grant-1", providerKind: "custom", ...next } };
  });
  return { call } as unknown as Pick<LightspeedClient, "call"> & { call: typeof call };
}

describe("grant leases", () => {
  it("caches until expiry minus the margin and single-flights concurrent leases", async () => {
    let now = 1_000_000;
    const rpc = client([
      { token: "t1", expiresAtMs: now + 120_000 },
      { token: "t2", expiresAtMs: null },
    ]);
    const lease = new GrantLease(rpc, "grant-1", () => now);
    await expect(Promise.all([lease.get(), lease.get()])).resolves.toEqual(["t1", "t1"]);
    expect(rpc.call).toHaveBeenCalledOnce();
    now += 120_000 - LEASE_EXPIRY_MARGIN_MS - 1;
    await expect(lease.get()).resolves.toBe("t1");
    now += 1;
    await expect(lease.get()).resolves.toBe("t2");
    expect(rpc.call).toHaveBeenCalledTimes(2);
  });

  it("holds a token without expiry for at most five minutes", async () => {
    let now = 5_000;
    const rpc = client([{ token: "t1" }, { token: "t2" }]);
    const lease = new GrantLease(rpc, "grant-1", () => now);
    await expect(lease.get()).resolves.toBe("t1");
    now += LEASE_MAX_AGE_MS - 1;
    await expect(lease.get()).resolves.toBe("t1");
    now += 1;
    await expect(lease.get()).resolves.toBe("t2");
  });

  it("re-leases after the provider rejected the token", async () => {
    const rpc = client([{ token: "t1", expiresAtMs: Date.now() + 3_600_000 }, { token: "t2" }]);
    const lease = new GrantLease(rpc, "grant-1");
    await expect(lease.get()).resolves.toBe("t1");
    lease.invalidate();
    await expect(lease.get()).resolves.toBe("t2");
    expect(rpc.call).toHaveBeenCalledTimes(2);
  });

  it("rejects an empty lease", async () => {
    const lease = new GrantLease(client([{ token: "" }]), "grant-1");
    await expect(lease.get()).rejects.toThrow(/no token/);
  });
});
