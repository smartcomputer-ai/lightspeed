// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { DisabledFeature } from "./disabled-feature";
import { UniverseGeneralSettingsPage } from "@/pages/UniverseGeneralSettingsPage";

const mocks = vi.hoisted(() => ({
  api: vi.fn(),
  admin: true,
  features: { bots: true } as Record<string, boolean>,
}));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/permissions", async (original) => ({
  ...await original<typeof import("@/lib/permissions")>(),
  useActionPermissions: () => ({ can: (action: string) => action !== "administer" || mocks.admin, isLoading: false }),
}));
vi.mock("@/lib/universes", async (original) => {
  const universe = () => ({
    id: "universe", organizationId: "team", handle: "test", name: "Test", status: "active", lightspeedUniverseId: "engine", role: "owner", features: mocks.features,
  });
  return {
    ...await original<typeof import("@/lib/universes")>(),
    useActiveUniverse: () => ({ universe: universe(), handle: "test", isLoading: false }),
  };
});

let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  mocks.admin = true;
  mocks.features = { bots: true };
  mocks.api.mockReset().mockResolvedValue({});
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

async function render(node: React.ReactNode) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  await act(async () => root.render(<QueryClientProvider client={client}><MemoryRouter>{node}</MemoryRouter></QueryClientProvider>));
}

it("tells an admin where a switched-off feature is turned on", async () => {
  mocks.features = { bots: false };
  await render(<DisabledFeature feature="bots" handle="test" mayConfigure={mocks.admin} />);
  expect(container.textContent).toContain("Bots are turned off in this universe");
  expect(container.querySelector("a")?.getAttribute("href")).toBe("/u/test/settings/general");
});

it("tells a member an admin can turn it on", async () => {
  mocks.admin = false;
  mocks.features = { bots: false };
  await render(<DisabledFeature feature="bots" handle="test" mayConfigure={mocks.admin} />);
  expect(container.textContent).toContain("Bots are turned off in this universe");
  expect(container.textContent).toContain("A universe Owner can turn them on.");
  expect(container.querySelector("a")).toBeNull();
});

it("switches a feature from General settings", async () => {
  mocks.features = { bots: false };
  await render(<UniverseGeneralSettingsPage />);
  // Each switch keeps a hidden checkbox under its id, beside the switch itself.
  expect(container.querySelector<HTMLInputElement>("#feature-bots")!.disabled).toBe(false);
  const [bots] = [...container.querySelectorAll<HTMLElement>('[role="switch"]')];
  await act(async () => bots!.click());
  expect(mocks.api).toHaveBeenCalledWith("PATCH", "/api/ui/universes/universe", { features: { bots: true } });
});
