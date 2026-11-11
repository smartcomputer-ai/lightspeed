// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Environment, EnvironmentRegistrationKey, EnvironmentTemplate } from "@/api";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { EnvironmentsPage } from "./EnvironmentsPage";

const mocks = vi.hoisted(() => ({ api: vi.fn(), role: "viewer" }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({
  useActiveUniverse: () => ({ universe: { id: "universe", role: mocks.role }, handle: "test", isLoading: false }),
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
/// Declared under `key-id` as "build-01"; no daemon has registered yet.
const declaredSource: Extract<Environment["source"], { type: "registered" }> = {
  type: "registered", registrationKeyId: "key-id", name: "build-01", identityMode: "persistent",
  connected: false, connectionChangedAtMs: 1, lastSeenAtMs: 1,
};
const declared = environment("Declared box", declaredSource, "away");
const declaredWithDaemon = environment("Declared box", { ...declaredSource, daemonId: "daemon-1" }, "away");
const registrationKey: EnvironmentRegistrationKey = {
  registrationKeyId: "key-id", displayName: "Build machines", keyPrefix: "lsrk_abc", identityMode: "persistent",
  declaredOnly: true, ephemeralDisconnectGraceMs: 300_000, offlineCloseAfterMs: 600_000, status: "active",
  registeredEnvironmentCount: 1, activeEnvironmentCount: 1, createdAtMs: 1, updatedAtMs: 1,
};
let rows: Environment[];
let keys: EnvironmentRegistrationKey[];
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
  keys = [];
  mocks.api.mockReset().mockImplementation(async (method: string, path: string) => {
    if (method !== "GET") return {};
    if (isEnvironmentList(path)) return environmentPage(path);
    if (path.endsWith("/environment-registration-keys")) return keys;
    if (path.endsWith("/environment-templates")) return [template];
    if (path.endsWith("/secrets")) return { grants: [], providers: [] };
    if (path.endsWith("/hints")) return { devEnvdEndpoint: null };
    if (path.endsWith("/environment-provider-bindings") || path.endsWith("/credentials")) return [];
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
const buttonIn = (scope: ParentNode, label: string) =>
  [...scope.querySelectorAll("button")].find((button) => button.textContent === label)!;
const dialogTitled = (title: string) =>
  [...document.body.querySelectorAll<HTMLElement>('[role="dialog"], [role="alertdialog"]')].find((element) =>
    document.getElementById(element.getAttribute("aria-labelledby")!)?.textContent === title)!;
async function click(element: HTMLElement) {
  await act(async () => element.click());
  await settle();
}
async function type(id: string, value: string) {
  const field = document.getElementById(id) as HTMLInputElement;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(field, value);
    field.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
const writes = () => mocks.api.mock.calls.filter(([method]) => method !== "GET");
/// The enrollment section lists keys behind a disclosure.
async function showRegistrationKeys() {
  mocks.role = "operator";
  await show();
  await click([...container.querySelectorAll("button")].find((button) => button.textContent?.startsWith("Registration keys"))!);
}

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

it("shows a declared environment as waiting until its daemon registers", async () => {
  mocks.role = "operator";
  rows = [declared];
  await show();
  const table = container.querySelector('table[aria-label="Environments"]')!;
  expect(table.textContent).toContain("Disconnected");
  expect(table.textContent).toContain("Waiting for its daemon to register as build-01");
  expect(table.textContent).not.toContain("Last seen");
  await details("Declared box");
  expect(dialog().textContent).toContain("Registration name");
  expect(dialog().textContent).toContain("build-01");
  expect(dialog().textContent).toContain("None bound");
  expect(dialog().textContent).not.toContain("undefined");
  expect(dialog().textContent).not.toContain("Last seen");
  expect(buttonLabels()).toContain("Rename…");
  expect(buttonLabels()).not.toContain("Release daemon");
});

it("releases the daemon of a declared environment after confirmation", async () => {
  mocks.role = "operator";
  rows = [declaredWithDaemon, registered];
  await show();
  await details("Runner");
  expect(buttonLabels()).not.toContain("Release daemon");
  await closeDetails();
  await details("Declared box");
  expect(dialog().textContent).toContain("daemon-1");
  await click(buttonIn(dialog(), "Release daemon"));
  expect(writes()).toEqual([]);
  const confirm = dialogTitled("Release the daemon of Declared box?");
  expect(confirm.textContent).toContain("disconnected");
  rows = [declared, registered];
  await click(buttonIn(confirm, "Release daemon"));
  expect(writes()).toEqual([["POST", "/api/v1/universes/universe/environments/Declared%20box/release"]]);
  expect(dialogTitled("Release the daemon of Declared box?")).toBeUndefined();
  expect(dialog().textContent).toContain("None bound");
  expect(buttonLabels()).not.toContain("Release daemon");
});

it("keeps releasing a daemon with operators", async () => {
  rows = [declaredWithDaemon];
  await show();
  await details("Declared box");
  expect(dialog().textContent).toContain("daemon-1");
  expect(buttonLabels()).not.toContain("Release daemon");
});

it("keeps a key's declared-only policy when another field is edited", async () => {
  keys = [registrationKey, { ...registrationKey, registrationKeyId: "open-key", displayName: "Open pool", declaredOnly: undefined }];
  await showRegistrationKeys();
  const keyRow = (name: string) => [...container.querySelectorAll<HTMLElement>("div.rounded-xl")]
    .find((row) => row.querySelector("p")?.textContent === name)!;
  expect(keyRow("Build machines").textContent).toContain("declared environments only");
  expect(keyRow("Open pool").textContent).not.toContain("declared environments only");
  await click(buttonIn(keyRow("Build machines"), "Edit"));
  const declaredOnly = () => dialogTitled("Edit “Build machines”").querySelector<HTMLElement>('[role="switch"]')!;
  expect(declaredOnly().getAttribute("aria-checked")).toBe("true");
  expect(dialogTitled("Edit “Build machines”").textContent).toContain("Only admit declared environments");
  await type("registration-key-name", "Build fleet");
  await click(buttonIn(dialogTitled("Edit “Build machines”"), "Save"));
  expect(writes()).toEqual([["PUT", "/api/v1/universes/universe/environment-registration-keys/key-id", {
    displayName: "Build fleet", offlineCloseAfterMs: 600_000, declaredOnly: true,
  }]]);

  mocks.api.mockClear();
  await click(buttonIn(keyRow("Build machines"), "Edit"));
  await click(declaredOnly());
  expect(declaredOnly().getAttribute("aria-checked")).toBe("false");
  await click(buttonIn(dialogTitled("Edit “Build machines”"), "Save"));
  expect(writes()).toEqual([["PUT", "/api/v1/universes/universe/environment-registration-keys/key-id", {
    displayName: "Build machines", offlineCloseAfterMs: 600_000,
  }]]);
});

it("offers the declared-only policy for persistent keys only", async () => {
  keys = [{ ...registrationKey, identityMode: "ephemeral", declaredOnly: undefined, offlineCloseAfterMs: null }];
  await showRegistrationKeys();
  expect(buttonIn(container, "Declare environment")).toBeUndefined();
  await click(buttonIn(container, "Edit"));
  expect(document.getElementById("registration-key-grace")).not.toBeNull();
  expect(dialogTitled("Edit “Build machines”").querySelector('[role="switch"]')).toBeNull();
});

it("declares an environment under a persistent key", async () => {
  keys = [registrationKey];
  await showRegistrationKeys();
  await click(buttonIn(container, "Declare environment"));
  const declare = dialogTitled("Declare an environment under “Build machines”");
  expect(buttonIn(declare, "Declare").disabled).toBe(true);
  await type("declared-environment-id", "build-box-02");
  await type("declared-environment-name", " build-02 ");
  expect(buttonIn(declare, "Declare").disabled).toBe(false);
  await click(buttonIn(declare, "Declare"));
  expect(writes()).toEqual([["POST", "/api/v1/universes/universe/environments/registered", {
    environmentId: "build-box-02", registrationKeyId: "key-id", name: "build-02",
  }]]);
  expect(dialogTitled("Declare an environment under “Build machines”")).toBeUndefined();
  expect(listReads().length).toBeGreaterThan(1);
});
