import { afterEach, expect, it, vi } from "vitest";
import type { AppContext } from "./context.js";
import { syncUniverseHandles, UniverseHandleCacheConflict } from "./universe-handles.js";

afterEach(() => vi.unstubAllGlobals());
function fixture(cached: string[], actual: string[]) {
  const updates: string[] = [];
  const rows = cached.map((handle, index) => ({ handle, universe: {
    organizationId: `org-${index}`, lightspeedUniverseId: `uuid-${index}`, gatewayUrl: null,
  } }));
  const chain = { from: () => chain, innerJoin: async () => rows };
  const tx = { update: () => ({ set: ({ slug }: { slug: string }) => ({ where: async () => { updates.push(slug); } }) }) };
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
    expect(rpc.method).toBe("deployment/universes/list");
    return Response.json({ id: rpc.id, result: { result: {
      universes: actual.map((handle, index) => ({ universeId: `uuid-${index}`, handle })),
    }, notifications: [] } });
  }));
  const transaction = vi.fn(async (work: (value: typeof tx) => unknown) => work(tx));
  const ctx = { db: { select: () => chain, transaction }, env: { lightspeedApiUrl: "https://runtime.example/rpc", lightspeedApiKey: "lsk_fixture" } } as unknown as AppContext;
  return { ctx, updates, transaction };
}
it("refreshes renamed runtime handles atomically, including swapped cache values", async () => {
  const f = fixture(["one", "two"], ["two", "one"]);
  expect(await syncUniverseHandles(f.ctx)).toEqual({ updated: 2, skipped: 0 });
  expect(f.transaction).toHaveBeenCalledTimes(1);
  expect(f.updates.slice(0, 2).every(handle => handle.startsWith("handle-cache-"))).toBe(true);
  expect(f.updates.slice(2)).toEqual(["two", "one"]);
});
it("avoids writes when the cache is current", async () => {
  const f = fixture(["one"], ["one"]);
  await syncUniverseHandles(f.ctx);
  expect(f.transaction).not.toHaveBeenCalled();
});
it("refuses ambiguous Platform URLs instead of inventing a local suffix", async () => {
  const f = fixture(["one", "two"], ["same", "same"]);
  await expect(syncUniverseHandles(f.ctx)).rejects.toBeInstanceOf(UniverseHandleCacheConflict);
  expect(f.updates).toEqual([]);
});

it("reports sync failure without modifying the cache when runtime is unavailable", async () => {
  const f = fixture(["cached"], ["renamed"]);
  vi.mocked(fetch).mockRejectedValue(new TypeError("offline"));
  await expect(syncUniverseHandles(f.ctx)).rejects.toThrow();
  expect(f.updates).toEqual([]);
});

it("reports universes the runtime no longer has and keeps their cached handle", async () => {
  const f = fixture(["kept", "gone"], ["kept"]);
  expect(await syncUniverseHandles(f.ctx)).toEqual({ updated: 0, skipped: 1 });
  expect(f.updates).toEqual([]);
});
