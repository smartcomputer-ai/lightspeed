// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { selectOption } from "@/test/select-option";
import { AdminAuditPage } from "./AdminAuditPage";

const mocks = vi.hoisted(() => ({ api: vi.fn(), listUsers: vi.fn() }));
vi.mock("@/api", () => ({ api: mocks.api }));
vi.mock("@/auth", () => ({ authClient: { admin: { listUsers: mocks.listUsers } } }));
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mocks.api.mockReset().mockResolvedValue({ events: [
    { id: "purge", createdAt: "2026-10-06T10:00:00Z", action: "session.purge", actorId: "admin", targetId: "removed-session", outcome: "success" },
    { id: "access", createdAt: "2026-10-06T09:00:00Z", action: "member.add", actorId: "removed-user", targetId: "member", outcome: "success" },
  ], nextCursor: null });
  mocks.listUsers.mockReset().mockResolvedValue({ data: { users: [{ id: "admin", email: "admin@example.test" }, { id: "member", email: "member@example.test" }] } });
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  container = document.createElement("div"); document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  client.clear(); container.remove(); vi.useRealTimers(); vi.unstubAllGlobals();
});
async function show() {
  await act(async () => root.render(<QueryClientProvider client={client}><AdminAuditPage /></QueryClientProvider>));
  await settle();
}
async function settle() {
  for (let step = 0; step < 5; step++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });
}
it("loads audit events directly and resolves users while preserving deleted targets", async () => {
  await show();
  expect(mocks.api).toHaveBeenCalledWith("GET", "/api/ui/admin/audit?limit=50");
  expect(container.textContent).toContain("Audit log");
  const rows = [...container.querySelectorAll("tbody tr")].map((row) => row.textContent);
  expect(rows[0]).toContain("Session permanently deleted");
  expect(rows[0]).toContain("admin@example.test");
  expect(rows[0]).toContain("removed-session");
  expect(rows[1]).toContain("removed-user");
  expect(rows[1]).toContain("member@example.test");
});
it("shows an empty audit log explicitly", async () => {
  mocks.api.mockResolvedValue({ events: [], nextCursor: null });
  await show();
  expect(container.textContent).toContain("No audit events recorded.");
});
it("shows read errors instead of an empty log", async () => {
  mocks.api.mockRejectedValue(new Error("Audit service unavailable"));
  await show();
  expect(container.textContent).toContain("Audit service unavailable");
  expect(container.textContent).not.toContain("No audit events recorded.");
});

function button(name: string) {
  const element = [...container.querySelectorAll("button")].find((item) => item.textContent === name);
  if (!element) throw new Error(`Missing button: ${name}`);
  return element;
}
async function click(name: string) {
  await act(async () => button(name).click());
  await settle();
}
const olderEvent = { id: "older", createdAt: "2026-10-05T09:00:00Z", action: "user.suspend", actorId: "admin", targetId: "older-user", outcome: "success" };
it("browses older pages and returns to the cached newer page", async () => {
  const first = await mocks.api();
  mocks.api.mockClear().mockResolvedValueOnce({ ...first, nextCursor: "access" })
    .mockResolvedValueOnce({ events: [olderEvent], nextCursor: null });
  await show();
  expect(button("Previous").disabled).toBe(true);
  await click("Next");
  expect(mocks.api).toHaveBeenLastCalledWith("GET", "/api/ui/admin/audit?limit=50&cursor=access");
  expect(container.textContent).toContain("Page 2");
  expect(container.textContent).toContain("older-user");
  expect(container.textContent).not.toContain("removed-session");
  expect(button("Next").disabled).toBe(true);
  await click("Previous");
  expect(container.textContent).toContain("Page 1");
  expect(container.textContent).toContain("removed-session");
  expect(mocks.api).toHaveBeenCalledTimes(2);
});
it("prevents advancing with a stale cursor while the next page loads", async () => {
  const first = await mocks.api();
  let resolve!: (value: unknown) => void;
  mocks.api.mockClear().mockResolvedValueOnce({ ...first, nextCursor: "access" })
    .mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
  await show();
  await click("Next");
  expect(container.textContent).toContain("removed-session");
  expect(button("Next").disabled).toBe(true);
  expect(button("Previous").disabled).toBe(true);
  await click("Next");
  expect(mocks.api).toHaveBeenCalledTimes(2);
  await act(async () => resolve({ events: [olderEvent], nextCursor: null }));
  await settle();
  expect(container.textContent).toContain("older-user");
});
it("restarts at the newest events when the page size changes or refresh is clicked", async () => {
  const first = await mocks.api();
  mocks.api.mockClear().mockResolvedValue({ ...first, nextCursor: "access" });
  await show();
  await click("Next");
  await selectOption(container.querySelector<HTMLElement>("#audit-page-size")!, "25");
  await settle();
  expect(mocks.api).toHaveBeenLastCalledWith("GET", "/api/ui/admin/audit?limit=25");
  expect(container.textContent).toContain("Page 1");
  await click("Next");
  expect(container.textContent).toContain("Page 2");
  mocks.api.mockResolvedValue({ events: [olderEvent], nextCursor: null });
  await click("Refresh");
  expect(mocks.api).toHaveBeenLastCalledWith("GET", "/api/ui/admin/audit?limit=25");
  expect(container.textContent).toContain("Page 1");
  expect(container.textContent).toContain("older-user");
  expect(button("Next").disabled).toBe(true);
});
it("allows returning to the previous page after a page request fails", async () => {
  const first = await mocks.api();
  mocks.api.mockClear().mockResolvedValueOnce({ ...first, nextCursor: "access" })
    .mockRejectedValueOnce(new Error("Older events unavailable"));
  await show();
  await click("Next");
  expect(container.textContent).toContain("Older events unavailable");
  expect(button("Next").disabled).toBe(true);
  expect(button("Previous").disabled).toBe(false);
  await click("Previous");
  expect(container.textContent).toContain("removed-session");
  expect(container.textContent).not.toContain("Older events unavailable");
});
