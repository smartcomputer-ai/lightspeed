// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ChannelConnectorHealth, ChannelsStatus } from "@/api";
import { AdminChannelsPage } from "./AdminChannelsPage";
import { waitForUi } from "@/test/wait-for-ui";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));

const health = (overrides: Partial<ChannelConnectorHealth>): ChannelConnectorHealth => ({
  version: 1, provider: "telegram", accountId: "support", state: "ready", ingressConnected: true,
  activityWorkerReady: true, reconnectAttempts: 0, changedAtMs: 0, ...overrides,
});
const status: ChannelsStatus = {
  connectors: [
    {
      url: "http://telegram:8080", reachable: true, httpStatus: 200,
      health: {
        version: 1, state: "degraded", startedAtMs: 0, discovery: { passes: 1 },
        accounts: [
          health({ universeId: "core-factory", accountId: "support" }),
          health({ universeId: "core-unknown", accountId: "sales", state: "failed", ingressConnected: false, lastError: "token revoked" }),
        ],
      },
    },
    { url: "http://whatsapp:8080", reachable: true, httpStatus: 200, health: health({ provider: "whatsapp", accountId: "ops", activityWorkerReady: false, detail: "waiting for pairing" }) },
    { url: "http://idle:8080", reachable: true, httpStatus: 200 },
    { url: "http://down:8080", reachable: false, httpStatus: null, error: "connection refused" },
  ],
};
const accounts = {
  accounts: [
    { universeId: "core-factory", accountId: "support", displayName: "Support line", provider: "telegram", providerAccountId: "@support_bot" },
    { universeId: "core-gone", accountId: "legacy", displayName: "Legacy", provider: "whatsapp", providerAccountId: "+100", enabled: false },
  ],
};
let routes: Record<string, () => unknown>;
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  routes = {
    "/api/v1/universes": () => [{ id: "factory", lightspeedUniverseId: "core-factory", name: "Factory", handle: "factory", status: "active", features: {} }],
    "/api/v1/status/channels": () => status,
    "/api/v1/channel-accounts": () => accounts,
  };
  mocks.api.mockReset().mockImplementation(async (method: string, path: string) => {
    const route = routes[path];
    if (method !== "GET" || !route) throw new Error(`Unexpected request: ${method} ${path}`);
    return route();
  });
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  client.clear();
  container.remove();
  vi.unstubAllGlobals();
});

async function show() {
  await act(async () => root.render(<QueryClientProvider client={client}><AdminChannelsPage /></QueryClientProvider>));
}
const tables = () => [...container.querySelectorAll("table")];
const cells = (table: HTMLTableElement | undefined) =>
  [...table?.querySelectorAll("tbody tr") ?? []].map((row) => [...row.querySelectorAll("td")].map((cell) => cell.textContent?.trim()));

it("lists one health row per connector account, and the host itself when it reports none", async () => {
  await show();
  await waitForUi(() => expect(tables()).toHaveLength(2));
  expect(cells(tables()[0])).toEqual([
    ["telegram / support", "Factory", "ready", "yes", "yes", "—"],
    ["telegram / sales", "core-unknown", "failed", "no", "yes", "token revoked"],
    ["whatsapp / ops", "—", "ready", "yes", "no", "waiting for pairing"],
    ["http://idle:8080", "—", "no accounts", "—", "—", "—"],
    ["http://down:8080", "—", "unreachable", "—", "—", "connection refused"],
  ]);
});

it("lists the account inventory read-only, enabled unless stated otherwise", async () => {
  await show();
  await waitForUi(() => expect(tables()).toHaveLength(2));
  expect(cells(tables()[1])).toEqual([
    ["Support linesupport", "telegram@support_bot", "Factory", "enabled"],
    ["Legacylegacy", "whatsapp+100", "core-gone", "disabled"],
  ]);
  expect(container.querySelectorAll("tbody button")).toHaveLength(0);
});

it("says when no connector host is configured", async () => {
  routes["/api/v1/status/channels"] = () => ({ connectors: [] });
  await show();
  await waitForUi(() => expect(container.textContent).toContain("No connector hosts are configured."));
  expect(tables()).toHaveLength(1);
});

it("keeps the inventory when connector status is unavailable", async () => {
  routes["/api/v1/status/channels"] = () => { throw new Error("status endpoint down"); };
  await show();
  await waitForUi(() => expect(container.textContent).toContain("status endpoint down"));
  expect(tables()).toHaveLength(1);
  expect(cells(tables()[0])[0]?.[0]).toBe("Support linesupport");
});
