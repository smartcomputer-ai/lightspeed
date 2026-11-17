import { afterAll, beforeAll, beforeEach, expect, it } from "vitest";
import type { AuditPage } from "@lightspeed-ai/platform-shared";
import { schema } from "@lightspeed-ai/platform-db";
import { identityEnv, identityFixture, responseCookies } from "../test/identity-fixture.js";

let fixture: Awaited<ReturnType<typeof identityFixture>>;
let cookie: string;
const id = (n: number) => `00000000-0000-4000-8000-${n.toString().padStart(12, "0")}`;

beforeAll(async () => {
  fixture = await identityFixture({ ...identityEnv, oidc: null });
  const login = await fixture.request("/api/auth/sign-in/email", {
    email: identityEnv.adminEmail, password: identityEnv.adminPassword,
  });
  expect(login.status).toBe(200);
  cookie = responseCookies(login);
}, 30000);
afterAll(async () => { await fixture?.postgres.close(); });
beforeEach(async () => { await fixture.db.delete(schema.identityAudit); });

async function insert(n: number, timestamp = "2026-01-01T00:00:00.000000Z") {
  await fixture.postgres.query(
    "INSERT INTO identity_audit (id, created_at, action, outcome) VALUES ($1, $2, 'test.event', 'success')",
    [id(n), timestamp],
  );
}
async function page(query = ""): Promise<AuditPage> {
  const response = await fixture.request(`/api/ui/admin/audit${query}`, undefined, cookie);
  expect(response.status).toBe(200);
  return response.json() as Promise<AuditPage>;
}

it("pages tied timestamps without repeats or gaps and excludes newer inserts", async () => {
  await insert(4, "2025-12-31T23:59:59Z");
  await insert(1, "2026-01-01T00:00:00.123456Z");
  await insert(2, "2026-01-01T00:00:00.123789Z");
  await insert(3, "2026-01-01T00:00:00.123789Z");
  const first = await page("?limit=2");
  expect(first.events.map(event => event.id)).toEqual([id(3), id(2)]);
  expect(first.nextCursor).toBe(id(2));
  await insert(5, "2026-01-02T00:00:00Z");
  await insert(6, "2026-01-01T00:00:00.123789Z");
  const second = await page(`?limit=2&cursor=${first.nextCursor}`);
  expect(second.events.map(event => event.id)).toEqual([id(1), id(4)]);
  expect(second.nextCursor).toBeNull();
  expect((await page("?limit=2")).events.map(event => event.id)).toEqual([id(5), id(6)]);
  expect(await page(`?limit=2&cursor=${id(4)}`)).toEqual({ events: [], nextCursor: null });
});

it("defaults to fifty entries, supports bounded sizes, and returns the remaining tail", async () => {
  await fixture.db.insert(schema.identityAudit).values(Array.from({ length: 103 }, (_, n) => ({
    id: id(n + 1), createdAt: new Date("2026-01-01T00:00:00Z"), action: "test.event", outcome: "success",
  })));
  const first = await page();
  expect(first.events).toHaveLength(50);
  expect(first.nextCursor).toBe(id(54));
  const second = await page(`?cursor=${first.nextCursor}`);
  expect(second.events).toHaveLength(50);
  expect(second.nextCursor).toBe(id(4));
  const last = await page(`?cursor=${second.nextCursor}`);
  expect(last.events.map(event => event.id)).toEqual([id(3), id(2), id(1)]);
  expect(last.nextCursor).toBeNull();
  expect((await page("?limit=100")).events).toHaveLength(100);
  expect((await page("?limit=1")).events).toHaveLength(1);
});

it("returns an empty first page without a continuation", async () => {
  expect(await page()).toEqual({ events: [], nextCursor: null });
});

it.each(["limit=0", "limit=101", "limit=-1", "limit=1.5", "limit=abc", "limit=", "cursor=bad", "cursor=", `cursor=${id(999)}`])(
  "rejects invalid pagination: %s", async query => {
    const response = await fixture.request(`/api/ui/admin/audit?${query}`, undefined, cookie);
    expect(response.status).toBe(400);
    expect(await response.json()).toHaveProperty("error");
  },
);

it("requires authentication before reading audit events", async () => {
  expect((await fixture.request("/api/ui/admin/audit?limit=2")).status).toBe(401);
});
