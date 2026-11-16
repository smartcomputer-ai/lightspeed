// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AccountsPage } from "./AccountsPage";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { waitForUi } from "@/test/wait-for-ui";
const mocks = vi.hoisted(() => ({ api: vi.fn(), role: "member", canCreate: false }));
vi.mock("@/lib/platform-api", () => ({ platformApi: mocks.api, useAccounts: () => ({ data: { canCreate: mocks.canCreate, accounts: [{ id: "team", name: "Team", kind: "shared", enabled: true, role: mocks.role, universeQuota: 100, defaultProtected: true, requireProtected: false }] } }) }));
let root: Root; let container: HTMLDivElement; let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true); mocks.role = "member"; mocks.canCreate = false;
  mocks.api.mockReset().mockImplementation(async operation => operation.endsWith("members/list") ? { members: [] } : operation.endsWith("applications/list") ? { applications: [] } : {});
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } }); container = document.createElement("div"); document.body.append(container); root = createRoot(container);
});
afterEach(async () => { await act(async () => root.unmount()); client.clear(); container.remove(); vi.unstubAllGlobals(); });
async function show() { await act(async () => root.render(<QueryClientProvider client={client}><PermissionIdentityProvider userId="user" platformAdmin={false}><AccountsPage /></PermissionIdentityProvider></QueryClientProvider>)); }
it("lets a Member inspect policy without requesting forbidden account directories", async () => {
  await show(); expect(container.querySelector<HTMLInputElement>('[aria-label="Universe quota"]')?.disabled).toBe(true); expect(container.textContent).toContain("An Account Admin manages"); expect(mocks.api).not.toHaveBeenCalled();
});
it("lets an Account Admin create an application in the selected account", async () => {
  mocks.role = "admin"; await show();
  const input = container.querySelector<HTMLInputElement>('[aria-label="Application name"]')!;
  await act(async () => { Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, "Build worker"); input.dispatchEvent(new Event("input", { bubbles: true })); });
  await act(async () => input.closest("form")!.querySelector<HTMLButtonElement>('button[type="submit"]')!.click());
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("platform/accounts/applications/create", { accountId: "team", name: "Build worker", role: "member" }));
});

it("shows shared account creation when the server permits self-service", async () => {
  mocks.canCreate = true;
  await show();
  expect(container.textContent).toContain("New shared account");
  expect(container.querySelector<HTMLInputElement>('[aria-label="New account name"]')).not.toBeNull();
});
