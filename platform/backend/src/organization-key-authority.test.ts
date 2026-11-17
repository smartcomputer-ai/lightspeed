import { expect, it, vi } from "vitest";
import type { DBAdapter } from "@better-auth/core/db/adapter";
import { organizationKeyAuthority } from "./organization-key-authority.js";

const query = (userId = "user", organizationId = "organization") => ({ model: "member", where: [
  { field: "userId", value: userId }, { field: "organizationId", value: organizationId },
] });
it("delegates only the reviewed real user and organization inside the scoped plugin call", async () => {
  const source = { findOne: vi.fn(async () => null) } as unknown as DBAdapter;
  const authority = organizationKeyAuthority(); const adapter = authority.adapter(source);
  expect(await adapter.findOne(query())).toBeNull();
  await authority.run({ userId: "user", organizationId: "organization" }, async () => {
    expect(await adapter.findOne(query())).toMatchObject({ userId: "user", organizationId: "organization", role: "admin" });
    expect(await adapter.findOne(query("other"))).toBeNull();
    expect(await adapter.findOne(query("user", "other"))).toBeNull();
    expect(await adapter.findOne({ ...query(), model: "session" })).toBeNull();
    expect(await adapter.findOne({ ...query(), where: [...query().where, { field: "role", value: "owner" }] })).toBeNull();
    expect(await adapter.findOne({ ...query(), where: query().where.map(clause => ({ ...clause, connector: "OR" as const })) })).toBeNull();
  });
  expect(await adapter.findOne(query())).toBeNull();
});
it("does not share a delegated organization across concurrent calls", async () => {
  const authority = organizationKeyAuthority();
  const adapter = authority.adapter({ findOne: async () => null } as unknown as DBAdapter);
  await Promise.all(["a", "b"].map(organizationId => authority.run({ userId: "user", organizationId }, async () => {
    await Promise.resolve();
    expect(await adapter.findOne(query("user", organizationId))).toMatchObject({ organizationId: organizationId });
    expect(await adapter.findOne(query("user", organizationId === "a" ? "b" : "a"))).toBeNull();
  })));
});
