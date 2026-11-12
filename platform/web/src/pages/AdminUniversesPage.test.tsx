// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { EngineUniverse, Universe, UniverseReconcile } from "@/api";
import { AdminUniversesPage } from "./AdminUniversesPage";
import { waitForUi } from "@/test/wait-for-ui";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));

const universe = (id: string, overrides: Partial<Universe> = {}): Universe => ({
  id, lightspeedUniverseId: `core-${id}`, name: id, handle: id, gatewayUrl: null, status: "active",
  createdAt: "2026-01-01T00:00:00Z", role: null, features: { bots: true, channels: true }, ...overrides,
});
const orphan = (universeId: string, overrides: Partial<EngineUniverse> = {}): EngineUniverse => ({
  universeId, handle: `u-${universeId}`, sessions: 3, workspaces: 1, profiles: 2, blobBytes: 2048, createdAtMs: 0, ...overrides,
});
let universes: Universe[];
let reconcile: UniverseReconcile | Error;
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  universes = [
    universe("zeta", { name: "Zeta", role: "admin" }),
    universe("alpha", { name: "Alpha", status: "archived" }),
    universe("mid", { name: "Mid", features: { bots: false, channels: false } }),
  ];
  reconcile = {
    platform: [
      { id: "zeta", lightspeedUniverseId: "core-zeta", engine: "ok" },
      { id: "mid", lightspeedUniverseId: "core-mid", engine: "missing" },
      { id: "alpha", lightspeedUniverseId: "core-alpha", engine: "unchecked" },
    ],
    orphans: [orphan("core-orphan", { handle: "kept" }), orphan("core-bare", { blobBytes: 5 * 1024 * 1024 })],
  };
  mocks.api.mockReset().mockImplementation(async (method: string, path: string) => {
    if (method === "GET" && path === "/api/v1/universes") return universes;
    if (method === "GET" && path === "/api/v1/universes/reconcile") {
      if (reconcile instanceof Error) throw reconcile;
      return reconcile;
    }
    if (method === "POST" && path === "/api/v1/universes/sync-handles") return { updated: 2, skipped: 1 };
    if (method !== "GET") return {};
    throw new Error(`Unexpected request: ${method} ${path}`);
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

function Location() {
  return <p data-testid="location">{useLocation().pathname}</p>;
}
async function show() {
  await act(async () => root.render(
    <QueryClientProvider client={client}><MemoryRouter initialEntries={["/admin/universes"]}>
      <Routes>
        <Route path="/admin/universes" element={<AdminUniversesPage />} />
        <Route path="*" element={<Location />} />
      </Routes>
    </MemoryRouter></QueryClientProvider>,
  ));
  await waitForUi(() => expect(container.querySelectorAll("table").length).toBeGreaterThan(0));
}
const tables = () => [...container.querySelectorAll("table")];
const rows = (table = tables()[0]) => [...table?.querySelectorAll("tbody tr") ?? []] as HTMLTableRowElement[];
const row = (name: string) => rows().find((node) => node.querySelector("td")?.textContent?.startsWith(name))!;
const cells = (node: HTMLTableRowElement) => [...node.querySelectorAll("td")].map((cell) => cell.textContent?.trim());
const buttonIn = (scope: ParentNode, label: string) => [...scope.querySelectorAll<HTMLButtonElement>("button")].find((node) => node.textContent?.trim() === label);
const alertDialog = () => document.querySelector<HTMLElement>('[role="alertdialog"]');
const writes = () => mocks.api.mock.calls.filter(([method]) => method !== "GET");

it("lists every universe by name with its status, engine link, and role", async () => {
  await show();
  expect(rows().map((node) => cells(node).slice(0, 4))).toEqual([
    ["Alphaalpha", "archived", "unchecked", "—"],
    ["Midmid", "active", "missing", "—"],
    ["Zetazeta", "active", "ok", "admin"],
  ]);
});

it("offers recreate only where the engine tenant is missing, and restore and delete only once archived", async () => {
  await show();
  expect(buttonIn(row("Mid"), "Recreate on engine")).toBeDefined();
  expect(buttonIn(row("Zeta"), "Recreate on engine")).toBeUndefined();
  expect(buttonIn(row("Zeta"), "Archive")).toBeDefined();
  expect(buttonIn(row("Zeta"), "Delete")).toBeUndefined();
  expect(buttonIn(row("Alpha"), "Archive")).toBeUndefined();
  expect(buttonIn(row("Alpha"), "Restore")).toBeDefined();
  expect(buttonIn(row("Alpha"), "Delete")).toBeDefined();

  await act(async () => buttonIn(row("Mid"), "Recreate on engine")!.click());
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("POST", "/api/v1/universes/mid/engine", {}));
  await act(async () => buttonIn(row("Zeta"), "Archive")!.click());
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("PATCH", "/api/v1/universes/zeta", { status: "archived" }));
  await act(async () => buttonIn(row("Alpha"), "Restore")!.click());
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("PATCH", "/api/v1/universes/alpha", { status: "active" }));
});

it("purges an archived universe only after confirmation", async () => {
  await show();
  await act(async () => buttonIn(row("Alpha"), "Delete")!.click());
  await waitForUi(() => expect(alertDialog()?.textContent).toContain("Permanently delete Alpha?"));
  expect(writes()).toHaveLength(0);
  await act(async () => buttonIn(alertDialog()!, "Delete permanently")!.click());
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("DELETE", "/api/v1/universes/alpha"));
});

it("opens a universe at its landing page", async () => {
  await show();
  await act(async () => buttonIn(row("Mid"), "Open")!.click());
  await waitForUi(() => expect(container.querySelector('[data-testid="location"]')?.textContent).toBe("/u/mid/sessions"));
});

it("reports what a runtime handle sync updated and skipped", async () => {
  await show();
  await act(async () => buttonIn(container, "Sync from runtime")!.click());
  await waitForUi(() => expect(container.querySelector('[role="status"]')?.textContent).toContain("Synced runtime handles: 2 updated."));
  expect(container.querySelector('[role="status"]')?.textContent).toContain("1 missing from the runtime and skipped");
  expect(mocks.api).toHaveBeenCalledWith("POST", "/api/v1/universes/sync-handles", {});
});

it("lists engine-only universes with their size, and deletes one after confirmation", async () => {
  await show();
  expect(rows(tables()[1]).map(cells)).toEqual([
    ["core-orphan", "3", "1", "2", "2.0 KB", "AdoptDelete"],
    ["core-bare", "3", "1", "2", "5.0 MB", "AdoptDelete"],
  ]);
  await act(async () => buttonIn(rows(tables()[1])[1]!, "Delete")!.click());
  await waitForUi(() => expect(alertDialog()?.textContent).toContain("Purges 3 session(s) and 5.0 MB of blobs"));
  await act(async () => buttonIn(alertDialog()!, "Delete permanently")!.click());
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("DELETE", "/api/v1/universes/engine/core-bare"));
});

it("requires a display name before adopting", async () => {
  await show();
  await act(async () => buttonIn(rows(tables()[1])[1]!, "Adopt")!.click());
  await waitForUi(() => expect(document.querySelector("#adopt-name")).not.toBeNull());
  const form = document.querySelector<HTMLFormElement>('[role="dialog"] form')!;
  await act(async () => form.requestSubmit());
  expect(form.textContent).toContain("a name is required");
  expect(writes()).toHaveLength(0);
});

it("adopts an orphan under the runtime handle it already has", async () => {
  await show();
  await act(async () => buttonIn(rows(tables()[1])[0]!, "Adopt")!.click());
  await waitForUi(() => expect(document.querySelector<HTMLInputElement>("#adopt-handle")?.readOnly).toBe(true));
  expect(document.querySelector<HTMLInputElement>("#adopt-handle")!.value).toBe("kept");
  await type("#adopt-name", "Kept");
  await act(async () => document.querySelector<HTMLFormElement>('[role="dialog"] form')!.requestSubmit());
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("POST", "/api/v1/universes/adopt", { lightspeedUniverseId: "core-orphan", name: "Kept" }));
});

it("keeps the inventory when engine reconciliation is unavailable", async () => {
  reconcile = new Error("engine unreachable");
  await show();
  await waitForUi(() => expect(container.textContent).toContain("Engine reconciliation unavailable: engine unreachable"));
  expect(rows()).toHaveLength(3);
  expect(cells(row("Mid"))[2]).toBe("—");
  expect(tables()).toHaveLength(1);
});

async function type(selector: string, value: string) {
  const node = document.querySelector<HTMLInputElement>(selector)!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(node, value);
    node.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
