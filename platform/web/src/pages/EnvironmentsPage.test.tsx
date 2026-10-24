// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Environment } from "@/api";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { EnvironmentsPage } from "./EnvironmentsPage";

const mocks = vi.hoisted(() => ({ api: vi.fn(), role: "viewer" }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({
  useActiveUniverse: () => ({ universe: { id: "universe", role: mocks.role }, slug: "test", isLoading: false }),
}));

function environment(id: string, source: Environment["source"], status: Environment["status"] = "ready"): Environment {
  return {
    environmentId: id, displayName: id, source, status, desiredPower: "running",
    createdAtMs: 1, updatedAtMs: 1, requestId: id, publicIngressEnabled: false,
    incarnation: { incarnationId: id, createdAtMs: 1, updatedAtMs: 1, powerStates: ["running", "paused"] },
  };
}
const provisioned = environment("Build box", { type: "provisioned", providerId: "incus", bindingId: "binding" }, "paused");
const registered = environment("Runner", { type: "registered", daemonId: "daemon", registrationKeyId: "key-id", identityMode: "persistent" }, "offline");
const external = environment("External box", { type: "external", connection: { endpoint: "https://example.test", transport: "http" } });
let rows: Environment[];
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  mocks.role = "viewer";
  rows = [registered, external, provisioned];
  mocks.api.mockReset().mockImplementation(async (_method: string, path: string) => {
    if (path.endsWith("/environments")) return rows;
    if (path.endsWith("/secrets")) return { grants: [], providers: [] };
    if (path.endsWith("/hints")) return { devEnvdEndpoint: null };
    if (path.endsWith("/environment-provider-bindings") || path.endsWith("/environment-templates")
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

it("shows one sorted semantic inventory with source-specific signals and lazy modal details", async () => {
  await show();
  const table = container.querySelector('table[aria-label="Environments"]')!;
  expect([...table.querySelectorAll("thead th")].map((cell) => cell.textContent)).toEqual(["Environment", "Source", "Status", "Details"]);
  expect([...table.querySelectorAll("tbody tr")].map((row) => row.querySelector("td [title]")?.getAttribute("title"))).toEqual([
    "Build box", "External box", "Runner",
  ]);
  expect(table.textContent).toContain("Paused → running");
  expect(table.textContent).toContain("Never seen");
  expect(table.textContent).toContain("key-id");
  expect(mocks.api.mock.calls.some(([, path]) => path.endsWith("/credentials"))).toBe(false);
  expect(mocks.api.mock.calls.some(([, path]) => path.endsWith("/environment-registration-keys"))).toBe(false);
  expect(dialog()).toBeNull();
  const button = await details("Runner");
  expect(button.getAttribute("aria-haspopup")).toBe("dialog");
  expect(document.getElementById(dialog().getAttribute("aria-labelledby")!)?.textContent).toBe("Runner");
  expect(dialog().textContent).toContain("daemon");
  expect(dialog().closest("table")).toBeNull();
  expect(table.querySelectorAll("tbody tr")).toHaveLength(3);
  expect(table.textContent).not.toContain("Secret environment variables");
  expect(mocks.api.mock.calls.filter(([, path]) => path.endsWith("/credentials")).map(([, path]) => path)).toEqual([
    "/api/v1/universes/universe/environments/Runner/credentials",
  ]);
  expect(dialog().textContent).not.toContain("Closing spends");
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
  await show();
  await details("Build box");
  expect(dialog().textContent?.includes("Close environment")).toBe(role === "operator");
  expect(dialog().textContent?.includes("Idle policy…")).toBe(role === "operator");
  expect(container.textContent?.includes("Enrollment")).toBe(role === "operator");
  await closeDetails();
  await details("Runner");
  expect(dialog().textContent).not.toContain("Close environment");
  expect(dialog().textContent).not.toContain("Idle policy…");
  await closeDetails();
  await details("External box");
  expect(dialog().textContent).not.toContain("Close environment");
  expect(dialog().textContent).not.toContain("Idle policy…");
});

it("keeps open details current as the environment changes", async () => {
  await show();
  await details("Build box");
  expect(dialog().textContent).toContain("Waking");
  await act(async () => client.setQueryData(["environments", "universe"], rows.map((row) =>
    row.environmentId === provisioned.environmentId ? { ...row, status: "ready" } : row,
  )));
  await settle();
  expect(dialog().textContent).toContain("Ready");
  expect(dialog().textContent).not.toContain("Waking");
});

it("opens idle policy as a nested dialog and returns to environment details", async () => {
  mocks.role = "operator";
  await show();
  await details("Build box");
  const policyButton = [...dialog().querySelectorAll("button")].find((button) => button.textContent === "Idle policy…")!;
  await act(async () => policyButton.click());
  await settle();
  const policy = [...document.body.querySelectorAll<HTMLElement>('[role="dialog"]')].find((element) =>
    document.getElementById(element.getAttribute("aria-labelledby")!)?.textContent === "Idle policy for Build box",
  )!;
  expect(policy).toBeTruthy();
  const cancel = [...policy.querySelectorAll("button")].find((button) => button.textContent === "Cancel")!;
  await act(async () => cancel.click());
  await settle();
  expect(document.body.querySelectorAll('[role="dialog"]')).toHaveLength(1);
  expect(dialog().textContent).toContain("Build box");
});

it("distinguishes an empty inventory from all closed and preserves the closed toggle", async () => {
  rows = [{ ...registered, status: "closed" }];
  await show();
  expect(container.textContent).toContain("Every environment here is closed.");
  expect(container.textContent).not.toContain("No environments yet");
  await act(async () => container.querySelector<HTMLButtonElement>('[role="switch"]')!.click());
  await settle();
  expect(container.querySelector("table")?.textContent).toContain("Runner");
});

it("shows empty inventory independently of provider metadata failures", async () => {
  rows = [];
  const implementation = mocks.api.getMockImplementation()!;
  mocks.api.mockImplementation(async (method, path) => {
    if (path.endsWith("/environment-provider-bindings")) throw new Error("Provider offline");
    return implementation(method, path);
  });
  await show();
  expect(container.textContent).toContain("No environments yet");
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
  mocks.api.mockImplementation((method, path) => path.endsWith("/environments")
    ? new Promise((_, fail) => { reject = fail; }) : implementation(method, path));
  await show();
  expect(container.textContent).toContain("Loading…");
  expect(container.textContent).not.toContain("No environments yet");
  await act(async () => reject(new Error("Inventory unavailable")));
  await settle();
  expect(container.textContent).toContain("Environments unavailable");
  expect(container.textContent).not.toContain("No environments yet");
});

it("continues polling while power diverges", async () => {
  await show();
  const reads = () => mocks.api.mock.calls.filter(([, path]) => path.endsWith("/environments")).length;
  const before = reads();
  await act(async () => { await vi.advanceTimersByTimeAsync(10_000); });
  expect(reads()).toBeGreaterThan(before);
});
