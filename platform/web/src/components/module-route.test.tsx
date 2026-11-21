// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, useNavigate, type NavigateFunction } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "@/App";
import { WebModuleProvider, type UniverseWebModule } from "@/lib/web-modules";
import { organizationModulesKey } from "@/lib/organization-modules";
import { waitForUi } from "@/test/wait-for-ui";

vi.mock("@/auth.js", () => ({
  authClient: { useSession: () => ({ data: { user: { id: "user" }, session: { id: "s" } } }) },
  useLoginConfig: () => ({ data: {} }),
  isPlatformAdmin: () => false,
}));
vi.mock("@/components/app-shell", async () => ({ AppShell: (await import("react-router-dom")).Outlet }));
vi.mock("@/pages/WorkspacesPage", () => ({ WorkspacesPage: () => <div>Host workspace page</div> }));

const Page = vi.fn(() => <div>Sample module page</div>);
const sample: UniverseWebModule = { id: "sample", label: "Sample", icon: () => null, action: "read", routes: [{ path: "", Page }] };
let client: QueryClient;
let root: Root;
let container: HTMLDivElement;
let navigate: NavigateFunction;
function RouterControls() { navigate = useNavigate(); return null; }

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  client = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity, retry: false } } });
  client.setQueryData(["me", "user"], { user: { id: "user" } });
  client.setQueryData(["universes"], [
    { id: "alpha", handle: "alpha", organizationId: "first", status: "active", actions: ["read"], features: {} },
    { id: "beta", handle: "beta", organizationId: "second", status: "active", actions: ["read"], features: {} },
    { id: "gamma", handle: "gamma", organizationId: "first", status: "active", actions: [], features: {} },
  ]);
  client.setQueryData(organizationModulesKey("first"), { modules: [{ id: sample.id, enabled: true }] });
  client.setQueryData(organizationModulesKey("second"), { modules: [{ id: sample.id, enabled: false }] });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  Page.mockClear();
});
afterEach(async () => {
  await act(async () => root.unmount());
  client.clear();
  container.remove();
  vi.unstubAllGlobals();
});
async function mount(path: string, modules = [sample]) {
  await act(async () => root.render(<QueryClientProvider client={client}><MemoryRouter initialEntries={[path]}>
    <RouterControls /><WebModuleProvider value={modules}><App /></WebModuleProvider>
  </MemoryRouter></QueryClientProvider>));
}

it("resolves a universe's landing page and gates a direct link after changing organization", async () => {
  await mount("/u/alpha");
  await waitForUi(() => expect(container.textContent).toContain("Sample module page"));
  await act(async () => { await navigate("/u/beta/sample"); });
  expect(container.textContent).toContain("Sample module is unavailable");
  expect(container.textContent).not.toContain("Sample module page");
});

it("refuses a direct link without the caller's module action", async () => {
  await mount("/u/gamma/sample");
  expect(container.textContent).toContain("Sample module is unavailable");
  expect(Page).not.toHaveBeenCalled();
});

it("lands on a host page when every bundled module is disabled", async () => {
  await mount("/u/beta");
  await waitForUi(() => expect(container.textContent).toContain("Host workspace page"));
  expect(Page).not.toHaveBeenCalled();
});

it("uses an ordinary host page when a server-enabled module is absent from the build", async () => {
  await mount("/u/alpha/sample", []);
  await waitForUi(() => expect(container.textContent).toContain("Host workspace page"));
  expect(Page).not.toHaveBeenCalled();
});
