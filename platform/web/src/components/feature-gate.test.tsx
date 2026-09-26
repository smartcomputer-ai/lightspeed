// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { FeatureGate } from "./feature-gate";
import { GeneralSettingsPage } from "@/pages/GeneralSettingsPage";
import { TriggerKindPicker } from "@/components/bot/triggers";

const mocks = vi.hoisted(() => ({
  api: vi.fn(),
  admin: true,
  features: { bots: true, channels: true } as Record<string, boolean>,
}));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/permissions", async (original) => ({
  ...await original<typeof import("@/lib/permissions")>(),
  useActionPermissions: () => ({ can: (action: string) => action !== "manage_access" || mocks.admin, isLoading: false }),
}));
vi.mock("@/lib/universes", async (original) => {
  const universe = () => ({
    id: "universe", slug: "test", name: "Test", status: "active", lightspeedUniverseId: "engine", role: "admin", features: mocks.features,
  });
  return {
    ...await original<typeof import("@/lib/universes")>(),
    useActiveUniverse: () => ({ universe: universe(), slug: "test", isLoading: false }),
    useFeature: (feature: string) => mocks.features[feature],
  };
});

let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  mocks.admin = true;
  mocks.features = { bots: true, channels: true };
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

it("shows a feature's page while it is on", async () => {
  await render(<FeatureGate feature="bots"><p>Bot list</p></FeatureGate>);
  expect(container.textContent).toBe("Bot list");
});

it("tells an admin where a switched-off feature comes back on", async () => {
  mocks.features = { bots: false, channels: false };
  await render(<FeatureGate feature="bots"><p>Bot list</p></FeatureGate>);
  expect(container.textContent).toContain("Bots are turned off in this universe");
  expect(container.textContent).not.toContain("Bot list");
  expect(container.querySelector("a")?.getAttribute("href")).toBe("/u/test/settings/general");
});

it("tells a member an admin can turn it back on", async () => {
  mocks.admin = false;
  mocks.features = { bots: true, channels: false };
  await render(<FeatureGate feature="channels"><p>Accounts</p></FeatureGate>);
  expect(container.textContent).toContain("Channels are turned off in this universe");
  expect(container.textContent).toContain("A universe admin can turn them back on.");
  expect(container.querySelector("a")).toBeNull();
});

it("switches a feature from General settings, and holds a feature whose requirement is off", async () => {
  mocks.features = { bots: false, channels: false };
  await render(<GeneralSettingsPage admin={false} />);
  // Each switch keeps a hidden checkbox under its id, beside the switch itself.
  expect(container.querySelector<HTMLInputElement>("#feature-channels")!.disabled).toBe(true);
  expect(container.querySelector<HTMLInputElement>("#feature-bots")!.disabled).toBe(false);
  const [bots] = [...container.querySelectorAll<HTMLElement>('[role="switch"]')];
  expect(container.textContent).toContain("Needs Bots.");
  await act(async () => bots!.click());
  expect(mocks.api).toHaveBeenCalledWith("PATCH", "/api/v1/universes/universe", { features: { bots: true } });
});

it("offers chat accounts as a bot trigger only while channels are on", async () => {
  await render(<TriggerKindPicker env={{ kind: "none" }} onPick={() => undefined} />);
  expect(container.textContent).toContain("Chat account");
  mocks.features = { bots: true, channels: false };
  await render(<TriggerKindPicker env={{ kind: "none" }} onPick={() => undefined} />);
  expect(container.textContent).not.toContain("Chat account");
  expect(container.textContent).toContain("Schedule");
});
