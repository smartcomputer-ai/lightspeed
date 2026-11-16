import { expect, it, vi } from "vitest";
import type { DBAdapter } from "@better-auth/core/db/adapter";
import { organizationKeyAuthority } from "./organization-key-authority.js";

const query = (userId = "user", accountId = "account") => ({ model: "member", where: [
  { field: "userId", value: userId }, { field: "organizationId", value: accountId },
] });
it("delegates only the reviewed real user and account inside the scoped plugin call", async () => {
  const source = { findOne: vi.fn(async () => null) } as unknown as DBAdapter;
  const authority = organizationKeyAuthority(); const adapter = authority.adapter(source);
  expect(await adapter.findOne(query())).toBeNull();
  await authority.run({ userId: "user", accountId: "account" }, async () => {
    expect(await adapter.findOne(query())).toMatchObject({ userId: "user", organizationId: "account", role: "admin" });
    expect(await adapter.findOne(query("other"))).toBeNull();
    expect(await adapter.findOne(query("user", "other"))).toBeNull();
    expect(await adapter.findOne({ ...query(), model: "session" })).toBeNull();
    expect(await adapter.findOne({ ...query(), where: [...query().where, { field: "role", value: "owner" }] })).toBeNull();
    expect(await adapter.findOne({ ...query(), where: query().where.map(clause => ({ ...clause, connector: "OR" as const })) })).toBeNull();
  });
  expect(await adapter.findOne(query())).toBeNull();
});
it("does not share a delegated account across concurrent calls", async () => {
  const authority = organizationKeyAuthority();
  const adapter = authority.adapter({ findOne: async () => null } as unknown as DBAdapter);
  await Promise.all(["a", "b"].map(accountId => authority.run({ userId: "user", accountId }, async () => {
    await Promise.resolve();
    expect(await adapter.findOne(query("user", accountId))).toMatchObject({ organizationId: accountId });
    expect(await adapter.findOne(query("user", accountId === "a" ? "b" : "a"))).toBeNull();
  })));
});
