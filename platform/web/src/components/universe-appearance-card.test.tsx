// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Universe } from "@/api";
import { DEFAULT_UNIVERSE_ICON, RETIRED_UNIVERSE_ICONS, UNIVERSE_ICON_REPLACEMENTS, UNIVERSE_ICONS } from "@lightspeed-ai/platform-shared";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { GeneralSettingsPage } from "@/pages/GeneralSettingsPage";
import { UniverseAppearanceCard, label } from "./universe-appearance-card";
import { UNIVERSE_ILLUSTRATIONS, offeredUniverseIcon } from "./universe-illustrations";
import { waitForUi } from "@/test/wait-for-ui";
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

it.each(UNIVERSE_ICONS)("saves the %s icon and a color as each is chosen", async icon => {
  const iconLabel = `${label(icon)} icon`;
  function CachedIcon() {
    const universe = client.getQueryData<Universe[]>(["universes"])![0]!;
    return <UniverseIcon icon={universe.icon} iconColor={universe.iconColor} />;
  }
  await render(<UniverseAppearanceCard universe={mocks.universe} />);
  if (icon !== DEFAULT_UNIVERSE_ICON) {
    await click(iconLabel);
    expect(mocks.api).toHaveBeenLastCalledWith("PATCH", "/api/v1/universes/universe", { icon, iconColor: "default" });
  }
  await click("Blue color");
  expect(mocks.api).toHaveBeenLastCalledWith("PATCH", "/api/v1/universes/universe", { icon, iconColor: "blue" });
  expect(button(iconLabel).getAttribute("aria-pressed")).toBe("true");
  expect(button("Blue color").getAttribute("aria-pressed")).toBe("true");
  const preview = container.querySelector('[aria-label="Universe appearance preview"]')!;
  expect(preview.querySelector(`img[data-universe-icon=${icon}]`)?.getAttribute("src")).toBe(UNIVERSE_ILLUSTRATIONS[icon]);
  expect(preview.querySelector("[data-universe-icon-tint=blue]")?.getAttribute("style")).toContain("oklch(0.58 0.11 255)");
  // The icon choices stay uncolored; the color choices show the icon in each color.
  expect(button(iconLabel).querySelector("[data-universe-icon-tint]")).toBeNull();
  expect(button("Blue color").querySelector("[data-universe-icon-tint=blue]")).not.toBeNull();
  await waitForUi(() => expect(client.getQueryData<Universe[]>(["universes"])![0]).toMatchObject({ icon, iconColor: "blue" }));
  await render(<CachedIcon />);
  expect(container.querySelector(`img[data-universe-icon=${icon}]`)).not.toBeNull();
  expect(container.querySelector("[data-universe-icon-tint=blue]")).not.toBeNull();
});

it("has an illustration for every offered icon and offers every illustration", () => {
  expect(Object.keys(UNIVERSE_ILLUSTRATIONS).sort()).toEqual([...UNIVERSE_ICONS].sort());
});

it.each(RETIRED_UNIVERSE_ICONS)("draws the retired %s icon as its replacement (else the default), in its color, without offering it", async icon => {
  const drawn = UNIVERSE_ICON_REPLACEMENTS[icon] ?? DEFAULT_UNIVERSE_ICON;
  await render(<UniverseIcon icon={icon} iconColor="blue" />);
  expect(container.querySelector(`img[data-universe-icon=${drawn}]`)).not.toBeNull();
  expect(container.querySelector("[data-universe-icon-tint=blue]")).not.toBeNull();
  await render(<UniverseAppearanceCard universe={{ ...mocks.universe, icon }} />);
  expect(container.querySelector(`button[aria-label="${label(icon)} icon"]`)).toBeNull();
  expect(button(`${label(drawn)} icon`).getAttribute("aria-pressed")).toBe("true");
});

it("maps the demo universes' earlier glyphs to their illustrations", () => {
  expect(offeredUniverseIcon("code")).toBe("mobius");
  expect(offeredUniverseIcon("sparkles")).toBe("personal-assistant");
  // An earlier name an illustration now carries draws that illustration.
  expect(offeredUniverseIcon("shield")).toBe("shield");
  expect(offeredUniverseIcon("orbit")).toBe(DEFAULT_UNIVERSE_ICON);
});

it("replaces retired names only with offered icons", () => {
  for (const replacement of Object.values(UNIVERSE_ICON_REPLACEMENTS)) expect(UNIVERSE_ICONS).toContain(replacement);
});

it("puts back the saved appearance when a save fails, and saves again on the next choice", async () => {
  mocks.api.mockRejectedValueOnce(new Error("Unable to save"));
  await render(<UniverseAppearanceCard universe={mocks.universe} />);
  await click("Network icon");
  await waitForUi(() => expect(container.querySelector('[role="alert"]')?.textContent).toBe("Unable to save"));
  expect(button(`${label(DEFAULT_UNIVERSE_ICON)} icon`).getAttribute("aria-pressed")).toBe("true");
  expect(button("Network icon").getAttribute("aria-pressed")).toBe("false");
  expect(client.getQueryData<Universe[]>(["universes"])![0]).toMatchObject({ icon: "orbit", iconColor: "default" });
  await click("Network icon");
  await waitForUi(() => expect(client.getQueryData<Universe[]>(["universes"])![0]).toMatchObject({ icon: "network" }));
  expect(container.querySelector('[role="alert"]')).toBeNull();
});

it("saves without a save or reset button, and not for the current choice", async () => {
  await render(<UniverseAppearanceCard universe={mocks.universe} />);
  const labels = Array.from(container.querySelectorAll("button")).map(element => element.textContent);
  expect(labels).not.toContain("Save appearance");
  expect(labels).not.toContain("Reset to default");
  await click(`${label(DEFAULT_UNIVERSE_ICON)} icon`); await click("Default color");
  expect(mocks.api).not.toHaveBeenCalled();
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
  expect(button(`${label(DEFAULT_UNIVERSE_ICON)} icon`)).toBeDefined();
});

it("renders the default badge when appearance is absent", async () => {
  await render(<UniverseIcon />);
  expect(container.querySelector(`img[data-universe-icon=${DEFAULT_UNIVERSE_ICON}]`)?.getAttribute("src"))
    .toBe(UNIVERSE_ILLUSTRATIONS[DEFAULT_UNIVERSE_ICON]);
  // The default color leaves the illustration gray.
  expect(container.querySelector("[data-universe-icon-tint]")).toBeNull();
});
