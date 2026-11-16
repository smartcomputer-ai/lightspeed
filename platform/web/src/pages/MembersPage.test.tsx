// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { MembersPage } from "./MembersPage";
import { waitForUi } from "@/test/wait-for-ui";
const mocks = vi.hoisted(() => ({ api: vi.fn(), role: "owner" }));
vi.mock("@/lib/platform-api", () => ({ platformApi: mocks.api }));
vi.mock("@/lib/universes", () => ({ useActiveUniverse: () => ({ universe: { id: "platform-id", lightspeedUniverseId: "runtime-id", accountId: "account", handle: "test", role: mocks.role }, isLoading: false }) }));
const grants = [
  { id: "owner-grant", principal: { kind: "user", id: "alice" }, name: "Alice", role: "owner" },
  { id: "app-grant", principal: { kind: "application", id: "robot" }, name: "Build robot", role: "viewer" },
];
let root: Root; let container: HTMLDivElement; let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true); mocks.role = "owner";
  mocks.api.mockReset().mockImplementation(async (operation: string) => {
    if (operation === "platform/universes/grants/list") return { grants };
    if (operation === "platform/accounts/members/list") return { members: [{ userId: "alice", name: "Alice" }] };
    if (operation === "platform/accounts/applications/list") return { applications: [{ id: "robot", name: "Build robot", enabled: true }] };
    return {};
  });
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  container = document.createElement("div"); document.body.append(container); root = createRoot(container);
});
afterEach(async () => { await act(async () => root.unmount()); client.clear(); container.remove(); vi.unstubAllGlobals(); });
async function show() { await act(async () => root.render(<QueryClientProvider client={client}><PermissionIdentityProvider userId="alice" platformAdmin={false}><MembersPage admin={false} /></PermissionIdentityProvider></QueryClientProvider>)); }
async function choose(label: string, value: string) { const node = container.querySelector<HTMLSelectElement>(`select[aria-label="${label}"]`)!; await act(async () => { node.value = value; node.dispatchEvent(new Event("change", { bubbles: true })); }); }
it("changes an application's universe role using the native scoped grant API", async () => {
  await show(); await waitForUi(() => expect(container.textContent).toContain("Build robot"));
  await choose("Role for Build robot", "operator");
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("platform/universes/grants/put", { universe: "runtime-id", principal: { kind: "application", id: "robot" }, role: "operator" }));
});
it("preserves the displayed Owner when the server refuses demotion", async () => {
  const implementation = mocks.api.getMockImplementation()!;
  mocks.api.mockImplementation(async (operation, params) => { if (operation === "platform/universes/grants/put") throw new Error("Keep at least one explicit Owner"); return implementation(operation, params); });
  await show(); await waitForUi(() => expect(container.querySelector('[aria-label="Role for Alice"]')).not.toBeNull());
  await choose("Role for Alice", "viewer");
  await waitForUi(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("at least one explicit Owner"));
  expect(container.querySelector<HTMLSelectElement>('[aria-label="Role for Alice"]')?.value).toBe("owner");
});
it("grants an existing account application access without creating an account member", async () => {
  await show(); await waitForUi(() => expect(container.querySelector('[value="application:robot"]')).not.toBeNull());
  await choose("Account principal", "application:robot"); await choose("Universe role", "owner");
  await act(async () => container.querySelector<HTMLButtonElement>('button[type="submit"]')!.click());
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("platform/universes/grants/put", { universe: "runtime-id", principal: { kind: "application", id: "robot" }, role: "owner" }));
  expect(mocks.api.mock.calls.some(([operation]) => operation === "platform/accounts/members/put")).toBe(false);
});
it.each(["viewer", "contributor", "operator"])("does not fetch the grant directory for a %s", async role => {
  mocks.role = role; await show(); expect(container.textContent).toContain("Only a universe Owner"); expect(mocks.api).not.toHaveBeenCalled();
});
