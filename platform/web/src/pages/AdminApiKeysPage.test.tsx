// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ApiError } from "@/api";
import { groupsFor } from "@/lib/method-groups";
import { AdminApiKeysPage } from "./AdminApiKeysPage";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({
  useUniverses: () => ({ data: [{ id: "platform-universe", lightspeedUniverseId: "core-universe", name: "Factory", handle: "factory" }] }),
}));
// JSDOM has no popup layout; a native select stands in for the scope picker.
vi.mock("@/components/ui/select", async () => {
  const React = await import("react");
  const Part = () => null;
  return {
    Select: ({ value, onValueChange, children }: { value: string; onValueChange: (value: string) => void; children: ReactNode }) => {
      const options: string[] = [];
      const visit = (nodes: ReactNode) => React.Children.forEach(nodes, (child) => {
        if (!React.isValidElement(child)) return;
        const props = child.props as { value?: string; children?: ReactNode };
        if (props.value !== undefined) options.push(props.value);
        else if (props.children) visit(props.children);
      });
      visit(children);
      return <select aria-label="Reaches" value={value} onChange={(event) => onValueChange(event.target.value)}>
        {options.map((option) => <option key={option} value={option}>{option}</option>)}
      </select>;
    },
    SelectContent: ({ children }: { children?: ReactNode }) => <>{children}</>, SelectItem: Part, SelectTrigger: ({ children }: { children?: ReactNode }) => <>{children}</>, SelectValue: Part,
  };
});

let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
const keys = [
  { keyId: "platform", keyPrefix: "lsk_platform", displayName: "Platform", scope: { kind: "deployment" }, groups: ["session"], assertActor: true, createdAtMs: 1, createdBy: { kind: "local" } },
  { keyId: "api_key_0123456789abcdef0123456789abcdef", keyPrefix: "lsk_universe", displayName: "Agent", scope: { kind: "universe", universeId: "core-universe" }, groups: ["session"], assertActor: false, createdAtMs: 2, createdBy: { kind: "local" } },
];
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  mocks.api.mockReset().mockImplementation(async (method: string) => method === "POST"
    ? { apiKey: { keyPrefix: "lsk_new" }, secret: "lsk_new_secret" }
    : keys);
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
  for (let step = 0; step < 4; step++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });
}
const button = (label: string) => [...document.body.querySelectorAll<HTMLButtonElement>("button")].find((node) => node.textContent?.trim() === label);
const dialogText = () => document.querySelector('[role="dialog"]')?.textContent ?? "";

it("lists keys by what they reach and whether they speak for people", async () => {
  await act(async () => root.render(<QueryClientProvider client={client}><AdminApiKeysPage /></QueryClientProvider>));
  await settle();
  const rows = [...container.querySelectorAll("tbody tr")].map((row) => row.textContent);
  expect(rows[0]).toContain("Factory");
  expect(rows[1]).toContain("Deployment");
  expect(rows[1]).toContain("Asserts people");
});

it("mints a universe key with only universe groups, then shows its secret once", async () => {
  await act(async () => root.render(<QueryClientProvider client={client}><AdminApiKeysPage /></QueryClientProvider>));
  await settle();
  await act(async () => button("Create key")!.click());
  await settle();
  expect(dialogText()).toContain("Universes");
  const scope = document.querySelector<HTMLSelectElement>('[role="dialog"] select[aria-label="Reaches"]')!;
  await act(async () => { scope.value = "platform-universe"; scope.dispatchEvent(new Event("change", { bubbles: true })); });
  await settle();
  expect(dialogText()).not.toContain("Universes");
  const name = document.querySelector<HTMLInputElement>("#admin-key-name")!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(name, "Agent");
    name.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await act(async () => document.querySelector<HTMLFormElement>('[role="dialog"] form')!.requestSubmit());
  await settle();
  // The request names every group that was ticked; none is left to the server.
  expect(mocks.api).toHaveBeenCalledWith("POST", "/api/v1/admin/api-keys", {
    displayName: "Agent", scope: { kind: "universe", universeId: "platform-universe" }, groups: groupsFor("universe"), assertActor: false,
  });
  expect(groupsFor("universe").some((group) => group.startsWith("deployment/"))).toBe(false);
  expect(document.querySelector<HTMLInputElement>("#api-key-secret")?.value).toBe("lsk_new_secret");
});

it("sends only the groups left ticked", async () => {
  await act(async () => root.render(<QueryClientProvider client={client}><AdminApiKeysPage /></QueryClientProvider>));
  await settle();
  await act(async () => button("Create key")!.click());
  await settle();
  const name = document.querySelector<HTMLInputElement>("#admin-key-name")!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(name, "Narrow");
    name.dispatchEvent(new Event("input", { bubbles: true }));
  });
  // Base UI keeps the id on a hidden input beside the visible checkbox.
  const checkbox = (group: string) =>
    document.getElementById(`admin-key-group-${group}`)!.parentElement!.querySelector<HTMLElement>('[role="checkbox"]')!;
  expect(checkbox("deployment/api-keys").getAttribute("aria-checked")).toBe("true");
  await act(async () => checkbox("deployment/api-keys").click());
  await settle();
  expect(checkbox("deployment/api-keys").getAttribute("aria-checked")).toBe("false");
  await act(async () => document.querySelector<HTMLFormElement>('[role="dialog"] form')!.requestSubmit());
  await settle();
  expect(mocks.api).toHaveBeenCalledWith("POST", "/api/v1/admin/api-keys", {
    displayName: "Narrow", scope: { kind: "deployment" },
    groups: groupsFor("deployment").filter((group) => group !== "deployment/api-keys"), assertActor: false,
  });
});

it("mints a key under a chosen id, and reopens the id field when that id is taken", async () => {
  await act(async () => root.render(<QueryClientProvider client={client}><AdminApiKeysPage /></QueryClientProvider>));
  await settle();
  await act(async () => button("Create key")!.click());
  await settle();
  const type = async (id: string, value: string) => {
    const field = document.querySelector<HTMLInputElement>(`#${id}`)!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(field, value);
      field.dispatchEvent(new Event("input", { bubbles: true }));
    });
  };
  const toggle = () => document.querySelector<HTMLButtonElement>('[role="dialog"] [aria-label="Change key ID"]')!;
  const submit = async () => {
    await act(async () => document.querySelector<HTMLFormElement>('[role="dialog"] form')!.requestSubmit());
    await settle();
  };
  expect(dialogText()).toContain("Generated key ID");
  await type("admin-key-name", "Gate");
  await act(async () => toggle().click());
  await type("admin-key-id", "platform");
  await act(async () => toggle().click());
  expect(document.querySelector("#admin-key-id")).toBeNull();

  const taken = 'key ID "platform" is already in use; leave it blank to generate a new ID or choose another';
  mocks.api.mockRejectedValueOnce(new ApiError(409, { error: taken }));
  await submit();
  expect(dialogText()).toContain(taken);
  expect(document.querySelector("#admin-key-id")?.getAttribute("aria-invalid")).toBe("true");

  await type("admin-key-id", "gate");
  expect(dialogText()).not.toContain(taken);
  expect(dialogText()).toContain("Key ID gate");
  await submit();
  expect(mocks.api).toHaveBeenCalledWith("POST", "/api/v1/admin/api-keys", {
    displayName: "Gate", scope: { kind: "deployment" }, groups: groupsFor("deployment"), assertActor: false, keyId: "gate",
  });
  expect(document.querySelector<HTMLInputElement>("#api-key-secret")?.value).toBe("lsk_new_secret");
});

it("rotates a key after confirmation and clears the displayed secret on close", async () => {
  await act(async () => root.render(<QueryClientProvider client={client}><AdminApiKeysPage /></QueryClientProvider>));
  await settle();
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Rotate Agent"]')!.click());
  await settle();
  expect(dialogText()).toContain("will stop working immediately");
  expect(mocks.api.mock.calls.some(([method]) => method === "POST")).toBe(false);
  // Refetch replaces the row's prefix; the secret dialog must survive it.
  mocks.api.mockImplementation(async (method: string) => method === "POST"
    ? { apiKey: { keyPrefix: "lsk_new" }, secret: "lsk_new_secret" }
    : keys.map((key) => key.keyPrefix === "lsk_universe" ? { ...key, keyPrefix: "lsk_new" } : key));
  await act(async () => button("Rotate key")!.click());
  await settle();
  expect(mocks.api).toHaveBeenCalledWith("POST", "/api/v1/admin/api-keys/api_key_0123456789abcdef0123456789abcdef/rotate?universeId=core-universe");
  expect(document.querySelector<HTMLInputElement>("#api-key-secret")?.value).toBe("lsk_new_secret");
  await act(async () => button("I saved the key")!.click());
  await settle();
  expect(document.querySelector("#api-key-secret")).toBeNull();
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Rotate Agent"]')!.click());
  await settle();
  expect(document.querySelector("#api-key-secret")).toBeNull();
  expect(dialogText()).toContain("Rotate this API key?");
});
