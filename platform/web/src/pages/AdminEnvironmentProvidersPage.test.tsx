// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AdminEnvironmentProvidersPage } from "./AdminEnvironmentProvidersPage";
import { waitForUi } from "@/test/wait-for-ui";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
// JSDOM has no popup layout; a native select stands in for the universe picker.
vi.mock("@/components/ui/select", async () => {
  const React = await import("react");
  function SelectTrigger(_props: { children?: ReactNode }) {
    return null;
  }
  function Select({ value, onValueChange, children }: { value: string; onValueChange: (value: string) => void; children: ReactNode }) {
    const options: { value: string; label: ReactNode }[] = [];
    const visit = (nodes: ReactNode) => React.Children.forEach(nodes, (child) => {
      if (!React.isValidElement(child) || child.type === SelectTrigger) return;
      const props = child.props as { value?: string; children?: ReactNode };
      if (props.value !== undefined) options.push({ value: props.value, label: props.children });
      else if (props.children) visit(props.children);
    });
    visit(children);
    return <select aria-label="Universe" value={value} onChange={(event) => onValueChange(event.target.value)}>
      {options.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
    </select>;
  }
  const Part = () => null;
  return { Select, SelectTrigger, SelectContent: ({ children }: { children?: ReactNode }) => <>{children}</>, SelectItem: Part, SelectValue: Part };
});

type Binding = { providerId: string; enabled: boolean };
const provider = (providerId: string, overrides: Record<string, unknown> = {}) => ({
  providerId, controllerEndpoint: `wss://${providerId}.internal/control`, hasCredential: false,
  effectiveDataRouteBase: `wss://${providerId}.internal/routes`, ...overrides,
});
const universe = (name: string, bindings: Binding[], overrides: Record<string, unknown> = {}) => ({
  universeId: `platform-${name}`, lightspeedUniverseId: `core-${name}`, name, status: "active", bindings, error: null, ...overrides,
});
let providers: unknown[];
let bindings: unknown[];
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  providers = [
    provider("incus-b", { displayName: "Incus B", hasCredential: true, dataRouteBase: "wss://routes.internal", effectiveDataRouteBase: "wss://routes.internal" }),
    provider("incus-a"),
  ];
  bindings = [
    universe("Zeta", [{ providerId: "incus-a", enabled: true }]),
    universe("Alpha", [{ providerId: "incus-a", enabled: false }]),
    universe("Beta", []),
    universe("Archived", [], { status: "archived" }),
    universe("Broken", [], { error: "engine timeout" }),
  ];
  mocks.api.mockReset().mockImplementation(async (method: string, path: string) => {
    if (method === "GET" && path === "/api/v1/admin/environment-providers") return providers;
    if (method === "GET" && path === "/api/v1/admin/environment-provider-bindings") return bindings;
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

async function show() {
  await act(async () => root.render(<QueryClientProvider client={client}><AdminEnvironmentProvidersPage /></QueryClientProvider>));
  await waitForUi(() => expect(container.querySelectorAll("section").length).toBe(providers.length));
}
const sections = () => [...container.querySelectorAll<HTMLElement>("section")];
const section = (providerId: string) => sections().find((node) => node.textContent?.includes(providerId))!;
const buttonIn = (scope: ParentNode, label: string) => [...scope.querySelectorAll<HTMLButtonElement>("button")].find((node) => node.textContent?.trim() === label);
const dialog = () => document.querySelector<HTMLElement>('[role="dialog"]');
const alertDialog = () => document.querySelector<HTMLElement>('[role="alertdialog"]');
const writes = () => mocks.api.mock.calls.filter(([method]) => method !== "GET");
async function type(selector: string, value: string) {
  const node = document.querySelector<HTMLInputElement>(selector)!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(node, value);
    node.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
const submit = () => act(async () => dialog()!.querySelector("form")!.requestSubmit());

it("lists providers by id with their routes, credential, and bound universes", async () => {
  await show();
  expect(sections().map((node) => node.querySelector("h2")?.textContent)).toEqual(["incus-a", "Incus B"]);
  expect(section("incus-a").textContent).toContain("no credential");
  expect(section("incus-a").textContent).toContain("Data routes wss://incus-a.internal/routes (derived)");
  expect(section("incus-b").textContent).toContain("credential");
  expect(section("incus-b").textContent).not.toContain("(derived)");
  expect(section("incus-b").textContent).toContain("No universe is bound to this provider yet.");
  const bound = [...section("incus-a").querySelectorAll("tbody tr")].map((row) => row.querySelector("td")?.textContent);
  expect(bound).toEqual(["Alphacore-Alpha", "Zetacore-Zeta"]);
  expect(container.textContent).toContain("Bindings could not be read for: Broken (engine timeout)");
});

it("says when no provider is registered", async () => {
  providers = [];
  await act(async () => root.render(<QueryClientProvider client={client}><AdminEnvironmentProvidersPage /></QueryClientProvider>));
  await waitForUi(() => expect(container.textContent).toContain("No environment providers are registered."));
});

it.each([
  { name: "a missing id", fields: { "#provider-endpoint": "wss://x/control" }, error: "a provider id is required" },
  { name: "a missing endpoint", fields: { "#provider-id": "incus-c" }, error: "a controller endpoint is required" },
  { name: "an http endpoint", fields: { "#provider-id": "incus-c", "#provider-endpoint": "https://x/control" }, error: "must be a ws://, wss:// or unix:// URL" },
  { name: "an http data route", fields: { "#provider-id": "incus-c", "#provider-endpoint": "wss://x/control", "#provider-data-route-base": "http://x/routes" }, error: "the data route base must be a ws:// or wss:// URL" },
  { name: "a unix controller without data routes", fields: { "#provider-id": "incus-c", "#provider-endpoint": "unix:///run/incus.sock" }, error: "a unix:// controller needs a data route base" },
])("refuses to register a provider with $name", async ({ fields, error }) => {
  await show();
  await act(async () => buttonIn(container, "Register provider")!.click());
  await waitForUi(() => expect(dialog()?.textContent).toContain("Register environment provider"));
  for (const [selector, value] of Object.entries(fields)) await type(selector, value);
  await submit();
  expect(dialog()!.textContent).toContain(error);
  expect(writes()).toHaveLength(0);
});

it("registers a provider with only the fields that were filled in", async () => {
  await show();
  await act(async () => buttonIn(container, "Register provider")!.click());
  await waitForUi(() => expect(dialog()).not.toBeNull());
  await type("#provider-id", " incus-c ");
  await type("#provider-endpoint", " wss://incus-c.internal:19090/control ");
  expect(document.querySelector<HTMLInputElement>("#provider-data-route-base")!.placeholder).toBe("wss://incus-c.internal:19090/routes");
  await type("#provider-credential", "secret");
  await submit();
  await waitForUi(() => expect(dialog()).toBeNull());
  expect(mocks.api).toHaveBeenCalledWith("PUT", "/api/v1/admin/environment-providers/incus-c", {
    controllerEndpoint: "wss://incus-c.internal:19090/control",
    credential: "secret",
  });
});

it("edits a provider under its fixed id and can drop the stored credential", async () => {
  await show();
  await act(async () => buttonIn(section("incus-b"), "Edit")!.click());
  await waitForUi(() => expect(dialog()?.textContent).toContain("Edit Incus B"));
  expect(document.querySelector<HTMLInputElement>("#provider-id")!.disabled).toBe(true);
  const credential = document.querySelector<HTMLInputElement>("#provider-credential")!;
  expect(credential.placeholder).toBe("Unchanged");
  const clear = [...dialog()!.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')].find((node) => node.closest("label")?.textContent?.includes("Remove the stored credential"))!;
  await act(async () => clear.click());
  expect(credential.disabled).toBe(true);
  await act(async () => buttonIn(dialog()!, "Save")!.click());
  await waitForUi(() => expect(dialog()).toBeNull());
  expect(mocks.api).toHaveBeenCalledWith("PUT", "/api/v1/admin/environment-providers/incus-b", {
    displayName: "Incus B",
    controllerEndpoint: "wss://incus-b.internal/control",
    dataRouteBase: "wss://routes.internal",
    clearCredential: true,
  });
});

it("toggles a binding and deletes one only after confirmation", async () => {
  await show();
  const toggle = section("incus-a").querySelector<HTMLElement>('[aria-label="Enable incus-a for Alpha"]')!;
  expect(toggle.getAttribute("aria-checked")).toBe("false");
  await act(async () => toggle.click());
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("PUT", "/api/v1/admin/universes/core-Alpha/environment-provider-bindings/incus-a", { enabled: true }));

  const zeta = [...section("incus-a").querySelectorAll("tbody tr")].find((row) => row.textContent?.includes("Zeta"))!;
  await act(async () => buttonIn(zeta, "Delete")!.click());
  await waitForUi(() => expect(alertDialog()?.textContent).toContain("Unbind Zeta?"));
  expect(mocks.api.mock.calls.some(([method]) => method === "DELETE")).toBe(false);
  await act(async () => buttonIn(alertDialog()!, "Delete binding")!.click());
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("DELETE", "/api/v1/admin/universes/core-Zeta/environment-provider-bindings/incus-a"));
});

it("binds only active, readable universes that are not bound yet", async () => {
  await show();
  await act(async () => buttonIn(section("incus-a"), "Bind universe")!.click());
  await waitForUi(() => expect(dialog()?.textContent).toContain("Bind a universe to incus-a"));
  const picker = dialog()!.querySelector<HTMLSelectElement>('select[aria-label="Universe"]')!;
  expect([...picker.options].map((option) => option.textContent)).toEqual(["Select a universe", "Beta"]);
  await submit();
  expect(dialog()!.textContent).toContain("choose a universe");
  await act(async () => { picker.value = "core-Beta"; picker.dispatchEvent(new Event("change", { bubbles: true })); });
  await submit();
  await waitForUi(() => expect(dialog()).toBeNull());
  expect(mocks.api).toHaveBeenCalledWith("PUT", "/api/v1/admin/universes/core-Beta/environment-provider-bindings/incus-a", { enabled: true });
});

it("holds the bind action when every active universe is already bound", async () => {
  bindings = [universe("Zeta", [{ providerId: "incus-a", enabled: true }])];
  await show();
  await act(async () => buttonIn(section("incus-a"), "Bind universe")!.click());
  await waitForUi(() => expect(dialog()?.textContent).toContain("Every active universe on this deployment is already bound"));
  expect(buttonIn(dialog()!, "Bind")!.disabled).toBe(true);
});

it("deletes a provider after confirmation and shows why the engine refused", async () => {
  mocks.api.mockImplementation(async (method: string, path: string) => {
    if (method === "DELETE") throw new Error("provider still has bindings");
    return path.endsWith("/environment-providers") ? providers : bindings;
  });
  await show();
  await act(async () => buttonIn(section("incus-a"), "Delete")!.click());
  await waitForUi(() => expect(alertDialog()?.textContent).toContain("Delete provider incus-a?"));
  await act(async () => buttonIn(alertDialog()!, "Delete provider")!.click());
  await waitForUi(() => expect(container.textContent).toContain("provider still has bindings"));
  expect(mocks.api).toHaveBeenCalledWith("DELETE", "/api/v1/admin/environment-providers/incus-a");
});
