// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { HomeRedirect } from "./HomeRedirect";
import { waitForUi } from "@/test/wait-for-ui";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));

const universe = (handle: string, overrides: Record<string, unknown> = {}) => ({
  id: `id-${handle}`, lightspeedUniverseId: `core-${handle}`, name: handle, handle, gatewayUrl: null, status: "active",
  createdAt: "2026-01-01T00:00:00Z", role: "viewer", features: { bots: true, channels: true }, ...overrides,
});

let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  localStorage.clear();
  mocks.api.mockReset();
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  client.clear();
  container.remove();
  localStorage.clear();
  vi.unstubAllGlobals();
});

function Landed() {
  return <p data-testid="landed">{useLocation().pathname}</p>;
}
async function show(admin: boolean) {
  await act(async () => root.render(
    <QueryClientProvider client={client}><MemoryRouter initialEntries={["/"]}>
      <Routes>
        <Route path="/" element={<HomeRedirect admin={admin} />} />
        <Route path="*" element={<Landed />} />
      </Routes>
    </MemoryRouter></QueryClientProvider>,
  ));
}
const landed = () => container.querySelector('[data-testid="landed"]')?.textContent;

it("returns to the last-visited universe while it is still a membership", async () => {
  mocks.api.mockResolvedValue([universe("alpha"), universe("beta")]);
  localStorage.setItem("lightspeed:last-universe", "beta");
  await show(false);
  await waitForUi(() => expect(landed()).toBe("/u/beta/bots"));
});

it("falls back to the first active membership when the last universe is gone or not joined", async () => {
  mocks.api.mockResolvedValue([
    universe("archived", { status: "archived" }),
    universe("browsed", { role: null }),
    universe("member", { features: { bots: false, channels: false } }),
  ]);
  localStorage.setItem("lightspeed:last-universe", "browsed");
  await show(true);
  // Bots switched off lands on sessions instead.
  await waitForUi(() => expect(landed()).toBe("/u/member/sessions"));
});

it("offers an admin a first universe when there is none", async () => {
  mocks.api.mockResolvedValue([universe("browsed", { role: null })]);
  await show(true);
  await waitForUi(() => expect(container.textContent).toContain("No universes yet"));
  expect(container.textContent).toContain("Create your first universe");
  expect(landed()).toBeUndefined();
  const create = [...container.querySelectorAll("button")].find((node) => node.textContent?.includes("New universe"))!;
  await act(async () => create.click());
  await waitForUi(() => expect(document.querySelector('[role="dialog"]')?.textContent).toContain("New universe"));
});

it("tells a member who to ask, without a create action", async () => {
  mocks.api.mockResolvedValue([]);
  await show(false);
  await waitForUi(() => expect(container.textContent).toContain("Ask a platform or universe admin"));
  expect([...container.querySelectorAll("button")].some((node) => node.textContent?.includes("New universe"))).toBe(false);
});

it("shows why the universe list could not load", async () => {
  mocks.api.mockRejectedValue(new Error("universes unavailable"));
  await show(false);
  await waitForUi(() => expect(container.textContent).toContain("universes unavailable"));
  expect(landed()).toBeUndefined();
});
