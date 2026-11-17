// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { waitForUi } from "@/test/wait-for-ui";
import { SetupsPage } from "./SetupsPage";
const mocks = vi.hoisted(() => ({ api: vi.fn(), accountRole: "admin", owner: true }));
vi.mock("@/api", () => ({ api: mocks.api }));
vi.mock("@/lib/universes", () => ({ useActiveUniverse: () => ({ universe: { id: "u", accountRole: mocks.accountRole }, handle: "test", isLoading: false }) }));
vi.mock("@/lib/permissions", () => ({ useActionPermissions: () => ({ can: (action: string) => action !== "manageAccess" || mocks.owner }) }));
let element: HTMLDivElement; let root: Root; let client: QueryClient;
let status: "available" | "ready";
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mocks.accountRole = "admin"; mocks.owner = true; status = "available";
  mocks.api.mockReset().mockImplementation(async (method: string) => {
    if (method === "POST") status = "ready";
    const setup = { id: "configurator", name: "Configurator", description: "Configure this universe", version: 6, available: true, status, resources: status === "ready" ? { serverId: "configurator" } : undefined };
    return method === "GET" ? [setup] : setup;
  });
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  element = document.createElement("div"); document.body.append(element); root = createRoot(element);
});
afterEach(async () => { await act(async () => root.unmount()); client.clear(); element.remove(); vi.unstubAllGlobals(); });
async function show(admin = false) {
  await act(async () => root.render(<QueryClientProvider client={client}><SetupsPage admin={admin} /></QueryClientProvider>));
  await waitForUi(() => expect(element.querySelector("button")).not.toBeNull());
}
it("installs and rotates a managed application credential without asking for a key", async () => {
  await show();
  expect(element.querySelector("input")).toBeNull();
  await act(async () => element.querySelector("button")!.click());
  await waitForUi(() => expect(element.textContent).toContain("Reinstall and rotate credential"));
  expect(mocks.api).toHaveBeenCalledWith("POST", "/api/ui/universes/u/setups/configurator/install");
  expect(element.textContent).toContain("MCP server: configurator");
  await act(async () => element.querySelector("button")!.click());
  await waitForUi(() => expect(mocks.api.mock.calls.filter(([method]) => method === "POST")).toHaveLength(2));
});
it.each([{ accountRole: "creator", owner: true }, { accountRole: "admin", owner: false }])(
  "keeps installation unavailable without both account and universe authority: %j", async authority => {
    Object.assign(mocks, authority); await show();
    const button = element.querySelector("button")!;
    expect(button.disabled).toBe(true);
    await act(async () => button.click());
    expect(mocks.api.mock.calls.every(([method]) => method === "GET")).toBe(true);
  },
);
it("allows a Platform administrator to install", async () => {
  mocks.accountRole = "member"; await show(true);
  expect(element.querySelector("button")!.disabled).toBe(false);
});
