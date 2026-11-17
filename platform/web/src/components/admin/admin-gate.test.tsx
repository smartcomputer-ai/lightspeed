// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PermissionIdentityProvider, useActionPermissions } from "@/lib/permissions";
import { useActiveUniverse } from "@/lib/universes";
import { AdminGate, AdminIndexRedirect } from "./admin-gate";
import { waitForUi } from "@/test/wait-for-ui";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async original => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mocks.api.mockImplementation(async (_method, path) => {
    if (path === "/api/v1/platform/accounts") return { accounts: [{ id: "team", enabled: true, role: "member" }] };
    if (path === "/api/ui/universes") return [{ id: "owned", name: "Owned universe", handle: "owned", status: "active", role: "owner" }, { id: "readable", handle: "readable", status: "active", role: "viewer" }];
    throw new Error(`Unexpected request: ${path}`);
  });
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  container = document.createElement("div"); document.body.append(container); root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount()); client.clear(); container.remove(); vi.unstubAllGlobals();
});
function UniversePermissions() {
  const { universe } = useActiveUniverse();
  const permissions = useActionPermissions(universe?.id);
  return <p>{universe?.name}: {permissions.can("manageAccess") ? "editable" : "denied"}</p>;
}
async function show(path: string) {
  await act(async () => root.render(<QueryClientProvider client={client}><PermissionIdentityProvider userId="user" platformAdmin={false}>
    <MemoryRouter initialEntries={[path]}><Routes><Route element={<AdminGate />}>
      <Route path="/admin" element={<AdminIndexRedirect />} />
      <Route path="/admin/universes" element={<p>Universe inventory</p>} />
      <Route path="/admin/universes/:handle/access" element={<UniversePermissions />} />
      <Route element={<AdminGate scope="platform" />}><Route path="/admin/users" element={<p>Global users</p>} /></Route>
      <Route element={<AdminGate scope="accounts" />}><Route path="/admin/accounts" element={<p>Account inventory</p>} /></Route>
    </Route></Routes></MemoryRouter>
  </PermissionIdentityProvider></QueryClientProvider>));
}
it("lands a universe-only Owner on universe administration", async () => {
  await show("/admin"); await waitForUi(() => expect(container.textContent).toBe("Universe inventory"));
});
it.each(["users", "accounts"])("refuses a universe Owner's direct link to %s administration", async page => {
  await show(`/admin/${page}`);
  await waitForUi(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("do not have permission"));
  expect(container.textContent).not.toContain("Global users");
  expect(container.textContent).not.toContain("Account inventory");
});
it.each([["owned", "Owned universe: editable"], ["readable", ": denied"]])("resolves the explicit Admin universe route %s for permission checks", async (handle, expected) => {
  await show(`/admin/universes/${handle}/access`);
  await waitForUi(() => expect(container.textContent).toBe(expected));
});
