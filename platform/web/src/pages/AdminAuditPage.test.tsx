// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
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
  mocks.api.mockReset().mockResolvedValue([
    { id: "purge", createdAt: "2026-10-06T10:00:00Z", action: "session.purge", actorId: "admin", targetId: "removed-session", outcome: "success" },
    { id: "access", createdAt: "2026-10-06T09:00:00Z", action: "member.add", actorId: "removed-user", targetId: "member", outcome: "success" },
  ]);
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
  for (let step = 0; step < 5; step++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });
}
it("loads audit events directly and resolves users while preserving deleted targets", async () => {
  await show();
  expect(mocks.api).toHaveBeenCalledWith("GET", "/api/v1/admin/audit");
  expect(container.textContent).toContain("Audit log");
  const rows = [...container.querySelectorAll("tbody tr")].map((row) => row.textContent);
  expect(rows[0]).toContain("Session permanently deleted");
  expect(rows[0]).toContain("admin@example.test");
  expect(rows[0]).toContain("removed-session");
  expect(rows[1]).toContain("removed-user");
  expect(rows[1]).toContain("member@example.test");
});
it("shows an empty audit log explicitly", async () => {
  mocks.api.mockResolvedValue([]);
  await show();
  expect(container.textContent).toContain("No audit events recorded.");
});
it("shows read errors instead of an empty log", async () => {
  mocks.api.mockRejectedValue(new Error("Audit service unavailable"));
  await show();
  expect(container.textContent).toContain("Audit service unavailable");
  expect(container.textContent).not.toContain("No audit events recorded.");
});
