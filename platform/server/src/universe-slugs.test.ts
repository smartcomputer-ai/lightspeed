import { afterEach, expect, it, vi } from "vitest";
import type { AppContext } from "./context.js";
import { syncUniverseSlugs, UniverseSlugCacheConflict } from "./universe-slugs.js";

afterEach(() => vi.unstubAllGlobals());
function fixture(cached: string[], actual: (string | null)[]) {
  const updates: string[] = [];
  const rows = cached.map((slug, index) => ({ slug, universe: {
    organizationId: `org-${index}`, lightspeedUniverseId: `uuid-${index}`, gatewayUrl: null,
  } }));
  const chain = { from: () => chain, innerJoin: async () => rows };
  const tx = { update: () => ({ set: ({ slug }: { slug: string }) => ({ where: async () => { updates.push(slug); } }) }) };
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
    expect(rpc.method).toBe("deployment/universes/list");
    return Response.json({ id: rpc.id, result: { result: {
      universes: actual.map((slug, index) => ({ universeId: `uuid-${index}`, slug })),
    }, notifications: [] } });
  }));
  const transaction = vi.fn(async (work: (value: typeof tx) => unknown) => work(tx));
  const ctx = { db: { select: () => chain, transaction }, env: { lightspeedApiUrl: "https://runtime.example/rpc", lightspeedApiKey: "lsk_fixture" } } as unknown as AppContext;
  return { ctx, updates, transaction };
}
it("refreshes renamed runtime slugs atomically, including swapped cache values", async () => {
  const f = fixture(["one", "two"], ["two", "one"]);
  expect(await syncUniverseSlugs(f.ctx)).toEqual({ updated: 2, skipped: 0 });
  expect(f.transaction).toHaveBeenCalledTimes(1);
  expect(f.updates.slice(0, 2).every(slug => slug.startsWith("slug-cache-"))).toBe(true);
  expect(f.updates.slice(2)).toEqual(["two", "one"]);
});
it("avoids writes when the cache is current", async () => {
  const f = fixture(["one"], ["one"]);
  await syncUniverseSlugs(f.ctx);
  expect(f.transaction).not.toHaveBeenCalled();
});
it("refuses ambiguous Platform URLs instead of inventing a local suffix", async () => {
  const f = fixture(["one", "two"], ["same", "same"]);
  await expect(syncUniverseSlugs(f.ctx)).rejects.toBeInstanceOf(UniverseSlugCacheConflict);
  expect(f.updates).toEqual([]);
});

it("reports sync failure without modifying the cache when runtime is unavailable", async () => {
  const f = fixture(["cached"], ["renamed"]);
  vi.mocked(fetch).mockRejectedValue(new TypeError("offline"));
  await expect(syncUniverseSlugs(f.ctx)).rejects.toThrow();
  expect(f.updates).toEqual([]);
});

it("reports skipped missing or unnamed runtime universes", async () => {
  const f = fixture(["legacy", "gone"], [null]);
  expect(await syncUniverseSlugs(f.ctx)).toEqual({ updated: 0, skipped: 2 });
  expect(f.updates).toEqual([]);
});
