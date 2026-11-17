// @vitest-environment jsdom
import { act } from "react";
import { organizationActions, universeActions } from "@lightspeed-ai/platform-shared";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { EngineUniverse, Universe, UniverseReconcile } from "@/api";
import { AdminUniversesPage } from "./AdminUniversesPage";
import { UnclaimedUniversesPage } from "./UnclaimedUniversesPage";
import { waitForUi } from "@/test/wait-for-ui";

const mocks = vi.hoisted(() => ({ api: vi.fn(), platformAdmin: true }));
vi.mock("@/lib/permissions", () => ({
  useDeploymentPermissions: () => ({ can: () => mocks.platformAdmin }),
  organizationAllows: (view: { actions?: string[] } | undefined, action: string) => view?.actions?.includes(action) ?? false,
}));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));

const universe = (id: string, overrides: Partial<Universe> = {}): Universe => ({
  id, organizationId: "team", lightspeedUniverseId: `core-${id}`, name: id, handle: id, status: "active",
  createdAt: "2026-01-01T00:00:00Z", role: "owner", actions: [...universeActions(["owner"])], features: { bots: true }, ...overrides,
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
  mocks.platformAdmin = true;
  universes = [
    universe("zeta", { name: "Zeta", role: "owner", actions: [...universeActions(["owner"])] }),
    universe("alpha", { name: "Alpha", status: "archived" }),
    universe("mid", { name: "Mid", features: { bots: false } }),
  ];
  reconcile = {
    platform: [
      { id: "zeta", lightspeedUniverseId: "core-zeta", engine: "ok" },
      { id: "mid", lightspeedUniverseId: "core-mid", engine: "missing" },
      { id: "alpha", lightspeedUniverseId: "core-alpha", engine: "ok" },
    ],
    orphans: [orphan("core-orphan", { handle: "kept" }), orphan("core-bare", { blobBytes: 5 * 1024 * 1024 })],
  };
  mocks.api.mockReset().mockImplementation(async (method: string, path: string) => {
    if (path === "/api/v1/platform/organizations") return { organizations: [{ id: "organization", name: "Team", role: "admin", actions: [...organizationActions(["admin"])] }] };
    if (path === "/api/v1/platform/organizations/organization/members") return { members: [{ userId: "user", name: "User" }] };
    if (method === "GET" && path === "/api/ui/universes") return universes;
    if (method === "GET" && path.startsWith("/api/ui/universes/reconcile")) {
      if (reconcile instanceof Error) throw reconcile;
      return reconcile;
    }
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
async function show(path = "/admin/organizations/team/universes") {
  await act(async () => root.render(
    <QueryClientProvider client={client}><MemoryRouter initialEntries={[path]}>
      <Routes>
        <Route path="/admin/platform/unclaimed-universes" element={<UnclaimedUniversesPage />} />
        <Route path="/admin/organizations/:organizationId/universes" element={<AdminUniversesPage />} />
        <Route path="*" element={<Location />} />
      </Routes>
    </MemoryRouter></QueryClientProvider>,
  ));
  await waitForUi(() => expect(client.isFetching()).toBe(0));
}
const tables = () => [...container.querySelectorAll("table")];
const rows = (table = tables()[0]) => [...table?.querySelectorAll("tbody tr") ?? []] as HTMLTableRowElement[];
const row = (name: string) => rows().find((node) => node.querySelector("td")?.textContent?.startsWith(name))!;
const cells = (node: HTMLTableRowElement) => [...node.querySelectorAll("td")].map((cell) => cell.textContent?.trim());
const buttonIn = (scope: ParentNode, label: string) => [...scope.querySelectorAll<HTMLButtonElement>("button")].find((node) => node.textContent?.trim() === label);
const alertDialog = () => document.querySelector<HTMLElement>('[role="alertdialog"]');
const writes = () => mocks.api.mock.calls.filter(([method]) => method !== "GET");

it("lists engine-only universes with their size, and deletes one after confirmation", async () => {
  await show("/admin/platform/unclaimed-universes");
  expect(rows(tables()[0]).map(cells)).toEqual([
    ["kept", "core-orphan", "3", "1", "2", "2.0 KB", "AdoptDelete"],
    ["u-core-bare", "core-bare", "3", "1", "2", "5.0 MB", "AdoptDelete"],
  ]);
  await act(async () => buttonIn(rows(tables()[0])[1]!, "Delete")!.click());
  await waitForUi(() => expect(alertDialog()?.textContent).toContain("Permanently delete u-core-bare?"));
  await type("#delete-u-core-bare", "u-core-bare");
  await act(async () => buttonIn(alertDialog()!, "Delete permanently")!.click());
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("DELETE", "/api/ui/universes/engine/core-bare", { handle: "u-core-bare" }));
});

it("requires a nonempty name if the suggestion is cleared", async () => {
  await show("/admin/platform/unclaimed-universes");
  await act(async () => buttonIn(rows(tables()[0])[1]!, "Adopt")!.click());
  await waitForUi(() => expect(document.querySelector("#adopt-name")).not.toBeNull());
  await type("#adopt-name", "");
  const form = document.querySelector<HTMLFormElement>('[role="dialog"] form')!;
  await act(async () => form.requestSubmit());
  expect(document.querySelector<HTMLInputElement>("#adopt-name")!.validity.valueMissing).toBe(true);
  expect(writes()).toHaveLength(0);
});

it("adopts an orphan under the runtime handle it already has", async () => {
  await show("/admin/platform/unclaimed-universes");
  await act(async () => buttonIn(rows(tables()[0])[0]!, "Adopt")!.click());
  await waitForUi(() => expect(document.querySelector<HTMLInputElement>("#adopt-handle")?.readOnly).toBe(true));
  expect(document.querySelector<HTMLInputElement>("#adopt-handle")!.value).toBe("kept");
  expect(document.querySelector<HTMLInputElement>("#adopt-name")!.value).toBe("kept");
  await type("#adopt-name", "Kept");
  await act(async () => document.querySelector<HTMLFormElement>('[role="dialog"] form')!.requestSubmit());
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("POST", "/api/ui/universes/adopt", { lightspeedUniverseId: "core-orphan", name: "Kept", organizationId: "organization", initialOwner: { kind: "user", id: "user" } }));
});

async function type(selector: string, value: string) {
  const node = document.querySelector<HTMLInputElement>(selector)!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(node, value);
    node.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

it("keeps a Platform Admin's inventory and reconciliation scoped to the selected organization", async () => {
  universes = [universe("local", { name: "Local", role: "owner", actions: [...universeActions(["owner"])], organizationId: "team" }), universe("elsewhere", { name: "Elsewhere", role: "owner", actions: [...universeActions(["owner"])], organizationId: "other" })];
  await show("/admin/organizations/team/universes");
  expect(row("Local")).toBeDefined(); expect(row("Elsewhere")).toBeUndefined();
  expect(mocks.api).toHaveBeenCalledWith("GET", "/api/ui/universes/reconcile?organizationId=team");
  expect(container.textContent).not.toContain("core-orphan");
});


it("shows only unclaimed runtime universes in Platform administration", async () => {
  await show("/admin/platform/unclaimed-universes");
  expect(container.querySelector("h1")?.textContent).toBe("Unclaimed Universes");
  expect(tables()).toHaveLength(1);
  expect(container.textContent).not.toContain("Zeta");
  expect(buttonIn(container, "New universe")).toBeUndefined();
  expect(mocks.api.mock.calls.some(([, path]) => path === "/api/ui/universes")).toBe(false);
});

it.each([["Display name", "Display name"], [null, "kept"], ["  ", "kept"]])("suggests the runtime name %s with a handle fallback", async (displayName, expected) => {
  (reconcile as UniverseReconcile).orphans = [orphan("core-orphan", { handle: "kept", displayName })];
  await show("/admin/platform/unclaimed-universes");
  await act(async () => buttonIn(rows()[0]!, "Adopt")!.click());
  await waitForUi(() => expect(document.querySelector<HTMLInputElement>("#adopt-name")?.value).toBe(expected));
  await type("#adopt-name", "My custom name");
  await act(async () => { await client.invalidateQueries({ queryKey: ["universes-reconcile"] }); });
  expect(document.querySelector<HTMLInputElement>("#adopt-name")!.value).toBe("My custom name");
});

it("shows an empty state when all runtime universes are claimed", async () => {
  (reconcile as UniverseReconcile).orphans = [];
  await show("/admin/platform/unclaimed-universes");
  expect(container.textContent).toContain("No unclaimed universes.");
  expect(tables()).toHaveLength(0);
});

it("reports unavailable runtime inventory without claiming it is empty", async () => {
  reconcile = new Error("runtime unreachable");
  await show("/admin/platform/unclaimed-universes");
  expect(container.textContent).toContain("Runtime inventory unavailable: runtime unreachable");
  expect(container.textContent).not.toContain("No unclaimed universes.");
});

const statusOf = (name: string) => cells(row(name)).at(-2);
const openRow = (name: string) => act(async () => (row(name).querySelector("td") as HTMLElement).click());
const dialog = () => document.querySelector<HTMLElement>('[role="dialog"]')!;
const location = () => container.querySelector('[data-testid="location"]')?.textContent;

it("lists the organization's universes with role and status, and a shortcut into each", async () => {
  await show();
  expect(rows().map((node) => node.querySelector("td span[title]")?.getAttribute("title"))).toEqual(["Alpha", "Mid", "Zeta"]);
  expect(statusOf("Alpha")).toBe("Archived");
  expect(statusOf("Mid")).toBe("Missing on engine");
  expect(statusOf("Zeta")).toBe("Active");
  for (const node of rows()) expect(node.querySelectorAll("button")).toHaveLength(1);
  const open = row("Zeta").querySelector<HTMLAnchorElement>('a[aria-label="Open Zeta"]')!;
  expect(open.getAttribute("href")).toBe("/u/zeta/bots");
  await act(async () => open.click());
  expect(document.querySelector('[role="dialog"]')).toBeNull();
});

it("opens a universe's details with the way into it and its settings, and the engine repair when the runtime lost it", async () => {
  await show();
  expect(row("Zeta").querySelector("[data-universe-icon]")).not.toBeNull();
  expect(row("Zeta").textContent).toContain("zeta");
  await openRow("Zeta");
  expect(dialog().querySelector("h2 [data-universe-icon]")).not.toBeNull();
  expect(dialog().textContent).toContain(`Universe ID${universes.find((candidate) => candidate.name === "Zeta")!.lightspeedUniverseId}`);
  expect(dialog().querySelector('a[href="/u/zeta/bots"]')?.textContent).toBe("Open universe");
  expect(dialog().querySelector('a[href="/u/zeta/settings/general"]')?.textContent).toBe("Settings");
  expect(buttonIn(dialog(), "Recreate on engine")).toBeUndefined();
  await act(async () => document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })));
  await waitForUi(() => expect(document.querySelector('[role="dialog"]')).toBeNull());
  await openRow("Mid");
  await act(async () => buttonIn(dialog(), "Recreate on engine")!.click());
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("POST", "/api/ui/universes/mid/engine", {}));
});

it("shows deletion progress and offers no way into a universe being deleted", async () => {
  universes[0] = { ...universes[0]!, status: "deleting", deletion: { startedAtMs: 1, requestedBy: { kind: "local" }, step: "environments", detail: "Waiting for provider" } };
  await show();
  expect(statusOf("Zeta")).toBe("Deleting");
  expect(row("Zeta").querySelector("a")).toBeNull();
  await openRow("Zeta");
  expect(dialog().textContent).toContain("environments: Waiting for provider");
  expect(dialog().querySelector("a")).toBeNull();
});

it("keeps the inventory when engine reconciliation is unavailable", async () => {
  reconcile = new Error("engine unreachable");
  await show();
  await waitForUi(() => expect(container.textContent).toContain("Engine reconciliation unavailable: engine unreachable"));
  expect(rows()).toHaveLength(3);
  expect(statusOf("Mid")).toBe("Active");
});

it("shows an Owner only the universes they administer, without deployment controls", async () => {
  mocks.platformAdmin = false;
  universes = universes.map((item) => item.name === "Zeta" ? item : { ...item, role: "viewer", actions: [...universeActions(["viewer"])] });
  await show();
  expect(rows()).toHaveLength(1);
  expect(mocks.api.mock.calls.some(([, path]) => path.includes("reconcile"))).toBe(false);
  await openRow("Zeta");
  expect(location()).toBeUndefined();
  expect(dialog().querySelector('a[href="/u/zeta/settings/members"]')).not.toBeNull();
});

it("disables New universe and says why once the organization reaches its limit", async () => {
  const original = mocks.api.getMockImplementation()!;
  mocks.api.mockImplementation(async (method: string, path: string, ...rest: unknown[]) => path === "/api/v1/platform/organizations"
    ? { organizations: [{ id: "team", name: "Team", role: "admin", actions: [...organizationActions(["admin"])], universeQuota: 3, canCreateUniverse: false }] }
    : original(method, path, ...rest));
  await show();
  await waitForUi(() => expect(buttonIn(container, "New universe")).toBeDefined());
  expect(buttonIn(container, "New universe")!.disabled).toBe(true);
  expect(container.textContent).toContain("It has reached its limit of 3 universes.");
});
