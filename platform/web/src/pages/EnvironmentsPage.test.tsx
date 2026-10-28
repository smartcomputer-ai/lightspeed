// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Environment, EnvironmentTemplate } from "@/api";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { EnvironmentsPage } from "./EnvironmentsPage";

const mocks = vi.hoisted(() => ({ api: vi.fn(), role: "viewer" }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({
  useActiveUniverse: () => ({ universe: { id: "universe", role: mocks.role }, slug: "test", isLoading: false }),
}));

function environment(
  id: string,
  source: Environment["source"],
  status: Environment["status"] = "ready",
  state: Environment["state"] = "open",
): Environment {
  return {
    environmentId: id, displayName: id, source, state, status,
    stateChangedAtMs: 1, createdAtMs: 1, updatedAtMs: 1, requestId: id,
  };
}
const template: EnvironmentTemplate = {
  providerId: "incus", templateId: "dev", displayName: "Dev box", deprecated: false,
  powerStates: ["running", "paused"], publicIngress: true,
};
const provisioned = environment("Build box", {
  type: "provisioned", providerId: "incus", templateId: "dev", targetId: "target",
  observed: { phase: "active", power: "paused", observedAtMs: 1 },
}, "paused");
const registered = environment("Runner", {
  type: "registered", daemonId: "daemon", registrationKeyId: "key-id", identityMode: "persistent",
  connected: false, connectionChangedAtMs: 1, lastSeenAtMs: 1,
}, "away");
const external = environment("External box", { type: "external", endpoint: "ws://example.test/" });
let rows: Environment[];
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;

/// `GET /environments[?includeClosed=true&after=…]` serves one page of `rows`.
function environmentPage(path: string) {
  const query = new URLSearchParams(path.split("?")[1] ?? "");
  const includeClosed = query.get("includeClosed") === "true";
  return { environments: rows.filter((row) => includeClosed || row.state !== "closed"), nextAfter: null };
}
const isEnvironmentList = (path: string) => path.split("?")[0]!.endsWith("/environments");
const listReads = () => mocks.api.mock.calls.filter(([, path]) => isEnvironmentList(path)).map(([, path]) => path as string);

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  mocks.role = "viewer";
  rows = [registered, external, provisioned];
  mocks.api.mockReset().mockImplementation(async (_method: string, path: string) => {
    if (isEnvironmentList(path)) return environmentPage(path);
    if (path.endsWith("/environment-templates")) return [template];
    if (path.endsWith("/secrets")) return { grants: [], providers: [] };
    if (path.endsWith("/hints")) return { devEnvdEndpoint: null };
    if (path.endsWith("/environment-provider-bindings")
      || path.endsWith("/environment-registration-keys") || path.endsWith("/credentials")) return [];
    throw new Error(`Unexpected request: ${path}`);
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
  vi.useRealTimers();
  vi.unstubAllGlobals();
});
async function settle() {
  for (let i = 0; i < 5; i++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });
}
async function show() {
  await act(async () => root.render(
    <QueryClientProvider client={client}><PermissionIdentityProvider userId="user" platformAdmin={false}>
      <EnvironmentsPage admin={false} />
    </PermissionIdentityProvider></QueryClientProvider>,
  ));
  await settle();
}
async function details(name: string) {
  const button = container.querySelector<HTMLButtonElement>(`button[aria-label="Details for ${name}"]`)!;
  await act(async () => button.click());
  await settle();
  return button;
}
const dialog = () => document.body.querySelector<HTMLElement>('[role="dialog"]')!;
async function closeDetails() {
  await act(async () => dialog().querySelector<HTMLButtonElement>('[data-slot="dialog-close"]')!.click());
  await settle();
}
const buttonLabels = () => [...dialog().querySelectorAll("button")].map((button) => button.textContent);

it("shows one sorted semantic inventory with source-specific signals and lazy modal details", async () => {
  await show();
  const table = container.querySelector('table[aria-label="Environments"]')!;
  expect([...table.querySelectorAll("thead th")].map((cell) => cell.textContent)).toEqual(["Environment", "Source", "Status", "Details"]);
  expect([...table.querySelectorAll("tbody tr")].map((row) => row.querySelector("td [title]")?.getAttribute("title"))).toEqual([
    "Build box", "External box", "Runner",
  ]);
  expect(table.textContent).toContain("Paused");
  expect(table.textContent).toContain("incus · Dev box");
  expect(table.textContent).toContain("Disconnected");
  expect(table.textContent).toContain("Last seen");
  expect(table.textContent).toContain("key-id");
  expect(table.textContent).toContain("ws://example.test/");
  expect(listReads()).toEqual(["/api/v1/universes/universe/environments"]);
  expect(mocks.api.mock.calls.some(([, path]) => path.endsWith("/credentials"))).toBe(false);
  expect(mocks.api.mock.calls.some(([, path]) => path.endsWith("/environment-registration-keys"))).toBe(false);
  expect(dialog()).toBeNull();
  const button = await details("Runner");
  expect(button.getAttribute("aria-haspopup")).toBe("dialog");
  expect(document.getElementById(dialog().getAttribute("aria-labelledby")!)?.textContent).toBe("Runner");
  expect(dialog().textContent).toContain("daemon");
  expect(dialog().textContent).toContain("Persistent · waits for its daemon to reconnect");
  expect(dialog().closest("table")).toBeNull();
  expect(table.querySelectorAll("tbody tr")).toHaveLength(3);
  expect(table.textContent).not.toContain("Secret environment variables");
  expect(mocks.api.mock.calls.filter(([, path]) => path.endsWith("/credentials")).map(([, path]) => path)).toEqual([
    "/api/v1/universes/universe/environments/Runner/credentials",
  ]);
  expect(dialog().textContent).not.toContain("Close environment");
  await closeDetails();
  expect(dialog()).toBeNull();
  expect(document.activeElement).toBe(button);
  expect(table.querySelectorAll("tbody tr")).toHaveLength(3);
});

it("opens details from the row and dismisses them with Escape", async () => {
  await show();
  const cell = container.querySelector("tbody td")!;
  await act(async () => (cell as HTMLElement).click());
  await settle();
  expect(dialog().textContent).toContain("Build box");
  await act(async () => dialog().dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })));
  await settle();
  expect(dialog()).toBeNull();
});

it.each(["viewer", "operator"])("preserves source-specific controls for %s", async (role) => {
  mocks.role = role;
  const operator = role === "operator";
  await show();
  await details("Build box");
  expect(buttonLabels().includes("Close environment")).toBe(operator);
  expect(buttonLabels().includes("Rename…")).toBe(operator);
  // Power buttons offer the template's states other than the observed one.
  expect(buttonLabels().includes("Resume")).toBe(operator);
  expect(buttonLabels()).not.toContain("Pause");
  expect(buttonLabels()).not.toContain("Suspend");
  expect(buttonLabels().includes("Enable public ingress")).toBe(operator);
  expect(container.textContent?.includes("Enrollment")).toBe(operator);
  await closeDetails();
  for (const name of ["Runner", "External box"]) {
    await details(name);
    expect(buttonLabels().includes("Close environment")).toBe(operator);
    expect(buttonLabels().includes("Rename…")).toBe(operator);
    expect(buttonLabels()).not.toContain("Resume");
    expect(buttonLabels()).not.toContain("Enable public ingress");
    await closeDetails();
  }
});

it("keeps open details current as the environment changes", async () => {
  await show();
  await details("Build box");
  expect(dialog().textContent).toContain("Paused");
  await act(async () => client.setQueryData(["environments", "universe", "pages", false], {
    pages: [{
      environments: rows.map((row) => row.environmentId === provisioned.environmentId ? { ...row, status: "ready" } : row),
      nextAfter: null,
    }],
    pageParams: [null],
  }));
  await settle();
  expect(dialog().textContent).toContain("Ready");
  expect(dialog().textContent).not.toContain("Paused");
});

it("shows the lifecycle message in the inventory and the details", async () => {
  rows = [{ ...provisioned, state: "failed", status: "failed", stateMessage: "Image dev is missing" }];
  await show();
  const table = container.querySelector('table[aria-label="Environments"]')!;
  expect(table.textContent).toContain("Failed");
  expect(table.textContent).toContain("Image dev is missing");
  await details("Build box");
  expect(dialog().textContent).toContain("Failure");
  expect(dialog().textContent).toContain("Image dev is missing");
});

it("opens rename as a nested dialog and returns to environment details", async () => {
  mocks.role = "operator";
  await show();
  await details("Build box");
  const renameButton = [...dialog().querySelectorAll("button")].find((button) => button.textContent === "Rename…")!;
  await act(async () => renameButton.click());
  await settle();
  const rename = [...document.body.querySelectorAll<HTMLElement>('[role="dialog"]')].find((element) =>
    document.getElementById(element.getAttribute("aria-labelledby")!)?.textContent === "Rename Build box",
  )!;
  expect(rename).toBeTruthy();
  const cancel = [...rename.querySelectorAll("button")].find((button) => button.textContent === "Cancel")!;
  await act(async () => cancel.click());
  await settle();
  expect(document.body.querySelectorAll('[role="dialog"]')).toHaveLength(1);
  expect(dialog().textContent).toContain("Build box");
});

it("hides closed environments until asked and then shows why they closed", async () => {
  rows = [{ ...registered, state: "closed", status: "closed", closeReason: "keyRevoked", closedAtMs: 1 }];
  await show();
  expect(container.textContent).toContain("No open environments");
  expect(container.querySelector("table")).toBeNull();
  await act(async () => container.querySelector<HTMLButtonElement>('[role="switch"]')!.click());
  await settle();
  expect(listReads()).toContain("/api/v1/universes/universe/environments?includeClosed=true");
  const table = container.querySelector("table")!;
  expect(table.textContent).toContain("Runner");
  expect(table.textContent).toContain("Closed when its registration key was revoked");
  await details("Runner");
  expect(dialog().textContent).toContain("Close reason");
});

it("loads further pages on demand", async () => {
  const implementation = mocks.api.getMockImplementation()!;
  mocks.api.mockImplementation(async (method, path) => {
    if (!isEnvironmentList(path)) return implementation(method, path);
    return path.includes("after=page-2")
      ? { environments: [external], nextAfter: null }
      : { environments: [provisioned], nextAfter: "page-2" };
  });
  await show();
  expect(container.querySelectorAll("tbody tr")).toHaveLength(1);
  const more = [...container.querySelectorAll("button")].find((button) => button.textContent === "Load more")!;
  await act(async () => more.click());
  await settle();
  expect(container.querySelectorAll("tbody tr")).toHaveLength(2);
  expect([...container.querySelectorAll("button")].some((button) => button.textContent === "Load more")).toBe(false);
});

it("shows empty inventory independently of provider metadata failures", async () => {
  rows = [];
  const implementation = mocks.api.getMockImplementation()!;
  mocks.api.mockImplementation(async (method, path) => {
    if (path.endsWith("/environment-provider-bindings")) throw new Error("Provider offline");
    return implementation(method, path);
  });
  await show();
  expect(container.textContent).toContain("No open environments");
  expect(container.textContent).toContain("Provider bindings unavailable");
});

it("retains usable inventory when enrollment metadata fails", async () => {
  mocks.role = "operator";
  const implementation = mocks.api.getMockImplementation()!;
  mocks.api.mockImplementation(async (method, path) => {
    if (path.endsWith("/environment-registration-keys")) throw new Error("Keys unavailable");
    return implementation(method, path);
  });
  await show();
  expect(container.querySelector("table")?.textContent).toContain("Runner");
  expect(container.querySelector("table")?.textContent).toContain("key-id");
  expect(container.textContent).toContain("Registration keys unavailable");
});

it("shows loading and inventory failure without a false empty state", async () => {
  const implementation = mocks.api.getMockImplementation()!;
  let reject!: (error: Error) => void;
  mocks.api.mockImplementation((method, path) => isEnvironmentList(path)
    ? new Promise((_, fail) => { reject = fail; }) : implementation(method, path));
  await show();
  expect(container.textContent).toContain("Loading…");
  expect(container.textContent).not.toContain("No open environments");
  await act(async () => reject(new Error("Inventory unavailable")));
  await settle();
  expect(container.textContent).toContain("Environments unavailable");
  expect(container.textContent).not.toContain("No open environments");
});

it("polls only while Lightspeed drives a lifecycle transition", async () => {
  await show();
  const before = listReads().length;
  await act(async () => { await vi.advanceTimersByTimeAsync(10_000); });
  expect(listReads().length).toBe(before);
  rows = [{ ...provisioned, state: "provisioning", status: "provisioning" }];
  await act(async () => { await client.invalidateQueries({ queryKey: ["environments", "universe"] }); });
  await settle();
  const polling = listReads().length;
  await act(async () => { await vi.advanceTimersByTimeAsync(10_000); });
  expect(listReads().length).toBeGreaterThan(polling);
});
