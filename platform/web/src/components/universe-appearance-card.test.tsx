// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Universe } from "@/api";
import { UNIVERSE_ICONS } from "@lightspeed/platform-shared";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { GeneralSettingsPage } from "@/pages/GeneralSettingsPage";
import { UniverseAppearanceCard } from "./universe-appearance-card";
import { UniverseIcon } from "./universe-icon";

const mocks = vi.hoisted(() => ({ api: vi.fn(), universe: {} as Universe }));
vi.mock("@/api", async original => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", async original => ({
  ...await original<typeof import("@/lib/universes")>(),
  useActiveUniverse: () => ({ universe: mocks.universe, slug: mocks.universe.slug, isLoading: false }),
}));

let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mocks.universe = { id: "universe", lightspeedUniverseId: "runtime", name: "Test", slug: "test",
    gatewayUrl: null, status: "active", createdAt: "2026-01-01", role: "admin",
    features: { bots: true, channels: true }, icon: "orbit", iconColor: "default" };
  mocks.api.mockReset().mockImplementation(async (_method, _path, fields) => ({ ...mocks.universe, ...fields }));
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  client.setQueryData(["universes"], [mocks.universe]);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  client.clear(); container.remove(); vi.unstubAllGlobals();
});
async function render(node: ReactNode) {
  await act(async () => root.render(<QueryClientProvider client={client}><MemoryRouter>{node}</MemoryRouter></QueryClientProvider>));
}
function button(label: string) {
  const result = Array.from(container.querySelectorAll<HTMLButtonElement>("button"))
    .find(button => button.getAttribute("aria-label") === label || button.textContent === label);
  expect(result, `Missing button: ${label}`).toBeDefined();
  return result!;
}
async function click(label: string) { await act(async () => button(label).click()); }

it.each(UNIVERSE_ICONS)("previews the %s icon and saves it to the shared universe cache", async icon => {
  const iconLabel = `${icon.charAt(0).toUpperCase() + icon.slice(1)} icon`;
  function CachedIcon() {
    const universe = client.getQueryData<Universe[]>(["universes"])![0]!;
    return <UniverseIcon icon={universe.icon} iconColor={universe.iconColor} />;
  }
  await render(<UniverseAppearanceCard universe={mocks.universe} />);
  expect(button("Save appearance").disabled).toBe(true);
  await click(iconLabel); await click("Blue color");
  expect(button(iconLabel).getAttribute("aria-pressed")).toBe("true");
  expect(button("Blue color").getAttribute("aria-pressed")).toBe("true");
  const preview = container.querySelector('[aria-label="Universe appearance preview"]')!;
  expect(preview.querySelector(`.lucide-${icon}`)).not.toBeNull();
  expect(preview.querySelector("span[style]")?.getAttribute("style")).toContain("oklch(0.58 0.11 255)");
  expect(mocks.api).not.toHaveBeenCalled();
  await click("Save appearance");
  await vi.waitFor(() => expect(client.getQueryData<Universe[]>(["universes"])![0]).toMatchObject({ icon, iconColor: "blue" }));
  expect(mocks.api).toHaveBeenCalledWith("PATCH", "/api/v1/universes/universe", { icon, iconColor: "blue" });
  await render(<CachedIcon />);
  expect(container.querySelector(`.lucide-${icon}`)).not.toBeNull();
  expect(container.querySelector("span[style]")?.getAttribute("style")).toContain("oklch(0.58 0.11 255)");
});

it("keeps the saved appearance when a save fails and allows retry", async () => {
  mocks.api.mockRejectedValueOnce(new Error("Unable to save"));
  await render(<UniverseAppearanceCard universe={mocks.universe} />);
  await click("Star icon"); await click("Save appearance");
  await vi.waitFor(() => expect(container.querySelector('[role="alert"]')?.textContent).toBe("Unable to save"));
  expect(client.getQueryData<Universe[]>(["universes"])![0]).toMatchObject({ icon: "orbit", iconColor: "default" });
  expect(button("Star icon").getAttribute("aria-pressed")).toBe("true");
  await click("Save appearance");
  await vi.waitFor(() => expect(client.getQueryData<Universe[]>(["universes"])![0]).toMatchObject({ icon: "star" }));
});

it("restores defaults only after saving", async () => {
  mocks.universe = { ...mocks.universe, icon: "heart", iconColor: "pink" };
  await render(<UniverseAppearanceCard universe={mocks.universe} />);
  await click("Reset to default");
  expect(mocks.api).not.toHaveBeenCalled();
  expect(button("Orbit icon").getAttribute("aria-pressed")).toBe("true");
  expect(button("Default color").getAttribute("aria-pressed")).toBe("true");
  await click("Save appearance");
  expect(mocks.api).toHaveBeenCalledWith("PATCH", "/api/v1/universes/universe", { icon: "orbit", iconColor: "default" });
});

it.each(["viewer", "contributor", "operator"] as const)("hides General settings from a %s", async role => {
  mocks.universe.role = role;
  await render(<PermissionIdentityProvider userId="user" platformAdmin={false}><GeneralSettingsPage admin={false} /></PermissionIdentityProvider>);
  expect(container.textContent).not.toContain("Appearance");
  expect(container.querySelector("form")).toBeNull();
});

it.each([false, true])("offers appearance on General settings for an authorized admin (platform admin: %s)", async platformAdmin => {
  mocks.universe.role = platformAdmin ? null : "admin";
  await render(<PermissionIdentityProvider userId="user" platformAdmin={platformAdmin}><GeneralSettingsPage admin={platformAdmin} /></PermissionIdentityProvider>);
  expect(container.textContent).toContain("Appearance");
  expect(button("Save appearance")).toBeDefined();
});

it("renders the default badge when appearance is absent", async () => {
  await render(<UniverseIcon />);
  expect(container.querySelector(".lucide-orbit")).not.toBeNull();
  expect(container.querySelector(".bg-sidebar-primary")).not.toBeNull();
});
