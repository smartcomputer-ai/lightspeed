import { expect, it, vi } from "vitest";
import { schema } from "@lightspeed-ai/platform-db";
import { listUniverseAccesses } from "./routes/universes.js";
import type { AppContext } from "./context.js";
import { userSubject } from "../test/subject-fixture.js";

it("resolves a large deployment inventory in four batched queries without revealing ungranted universes", async () => {
  const accounts = Array.from({ length: 2000 }, (_, index) => ({ id: `account-${index}`, enabled: true, membershipPolicy: {} }));
  const universes = Array.from({ length: 2000 }, (_, index) => ({ id: crypto.randomUUID(), organizationId: "account-1", lightspeedUniverseId: crypto.randomUUID(), handle: `work-${index}`, status: "active", accessPolicy: {}, runtimeConfiguration: {} }));
  const tables = new Map<unknown, unknown[]>([
    [schema.organization, accounts],
    [schema.member, [{ organizationId: "account-1", userId: "caller", role: "creator" }]],
    [schema.universes, universes],
    [schema.universeGrants, [{ universeId: universes[0]!.id, role: "owner" }, { universeId: universes[1]!.id, role: "viewer" }]],
  ]);
  const select = vi.fn(() => ({ from(table: unknown) {
    const rows = tables.get(table); if (!rows) throw new Error("unexpected table");
    const query = { where: () => query, then: (resolve: (value: unknown[]) => unknown) => Promise.resolve(rows).then(resolve) };
    return query;
  } }));
  const result = await listUniverseAccesses({ db: { select } } as unknown as AppContext, userSubject());
  expect(select).toHaveBeenCalledTimes(4);
  expect(result.map(access => access.role)).toEqual(["owner", "viewer"]);
  expect(result.map(access => access.universe.id)).toEqual([universes[0]!.id, universes[1]!.id]);
});
