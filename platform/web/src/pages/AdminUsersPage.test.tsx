// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AdminUsersPage } from "./AdminUsersPage";
const mocks = vi.hoisted(() => ({ listUsers: vi.fn(), banUser: vi.fn(), unbanUser: vi.fn(), revokeUserSessions: vi.fn() }));
vi.mock("@/auth", () => ({
  useLoginConfig: () => ({ data: { sso: true, password: "break-glass" } }),
  authClient: { admin: mocks },
}));
let root: Root; let client: QueryClient; let container: HTMLDivElement;
beforeEach(() => {
  vi.useFakeTimers(); vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true); vi.stubGlobal("PointerEvent", MouseEvent);
  mocks.listUsers.mockResolvedValue({ data: { users: [{ id: "company-user", name: "Company User", email: "user@example.test", role: "user", identitySource: "company", companyAdmitted: true, providerCheckedAt: "2026-09-27T12:00:00Z" }] } });
  mocks.banUser.mockReset().mockResolvedValue({});
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  container = document.createElement("div"); document.body.append(container); root = createRoot(container);
});
afterEach(async () => { await act(async () => root.unmount()); client.clear(); container.remove(); vi.useRealTimers(); vi.unstubAllGlobals(); });
async function settle() { for (let i = 0; i < 5; i++) await act(async () => { await vi.advanceTimersByTimeAsync(5); }); }
it("shows company access and provides suspension without local role or password changes", async () => {
  await act(async () => root.render(<QueryClientProvider client={client}><AdminUsersPage currentUser={{ id: "admin", name: "Admin", email: "admin@example.test" }} /></QueryClientProvider>));
  await settle();
  expect(container.textContent).toContain("Company account");
  expect(container.textContent).toContain("Last company check");
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Edit user@example.test"]')!.click());
  await settle();
  const dialog = document.querySelector('[role="dialog"]')!;
  expect(dialog.textContent).toContain("managed by the company");
  expect(dialog.querySelector<HTMLInputElement>("#edit-user-email")!.disabled).toBe(true);
  expect(dialog.querySelector('input[type="password"]')).toBeNull();
  const suspend = [...dialog.querySelectorAll("button")].find((b) => b.textContent?.includes("Suspend access"))!;
  await act(async () => suspend.click());
  await settle();
  expect(mocks.banUser).toHaveBeenCalledWith({ userId: "company-user" });
});
