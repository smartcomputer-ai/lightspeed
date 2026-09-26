// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AdminApiKeysPage } from "./AdminApiKeysPage";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({
  useUniverses: () => ({ data: [{ id: "platform-universe", lightspeedUniverseId: "core-universe", name: "Factory", slug: "factory" }] }),
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
  { keyPrefix: "lsk_platform", displayName: "Platform", scope: { kind: "deployment" }, groups: ["session"], assertActor: true, createdAtMs: 1, createdBy: { kind: "local" } },
  { keyPrefix: "lsk_universe", displayName: "Agent", scope: { kind: "universe", universeId: "core-universe" }, groups: ["session"], assertActor: false, createdAtMs: 2, createdBy: { kind: "local" } },
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
  expect(mocks.api).toHaveBeenCalledWith("POST", "/api/v1/admin/api-keys", {
    displayName: "Agent", scope: { kind: "universe", universeId: "platform-universe" }, assertActor: false,
  });
  expect(document.querySelector<HTMLInputElement>("#api-key-secret")?.value).toBe("lsk_new_secret");
});
