// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { SessionView } from "@/api";
import { SessionSettingsDialog } from "./session-settings-sheet";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/permissions", () => ({ useActionPermissions: () => ({ can: () => true }) }));
vi.mock("@/lib/sessions/editor-options", () => ({ useSessionConfigEditorOptions: () => ({}) }));
// Every select becomes a native control; jsdom cannot lay out the popup.
vi.mock("@/components/ui/select", async () => {
  const React = await import("react");
  function Select({ value, onValueChange, children }: { value: string; onValueChange: (value: string) => void; children: ReactNode }) {
    const options: { value: string; label: ReactNode }[] = [];
    const visit = (nodes: ReactNode) =>
      React.Children.forEach(nodes, (child) => {
        if (!React.isValidElement(child)) return;
        const props = child.props as { value?: string; children?: ReactNode };
        if (props.value !== undefined) options.push({ value: props.value, label: props.children });
        else if (props.children) visit(props.children);
      });
    visit(children);
    return (
      <select value={value} onChange={(event) => onValueChange(event.target.value)}>
        {options.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
      </select>
    );
  }
  const Part = () => null;
  return { Select, SelectTrigger: Part, SelectContent: ({ children }: { children?: ReactNode }) => <>{children}</>, SelectItem: Part, SelectValue: Part };
});

const session = {
  id: "session",
  configRevision: 1,
  activeEnvironmentId: "machine-a",
  config: {
    model: { providerId: "openai", apiKind: "openai:responses", model: "saved-model" },
    features: { environments: { environments: [{ environmentId: "machine-a", access: "read" }], prompts: {}, skills: {} } },
  },
  metadata: {},
  retention: { rootSessionId: "session" },
} as unknown as SessionView;

let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mocks.api.mockReset().mockImplementation(async (method: string, path: string) => {
    if (path.endsWith("/instructions")) return { text: "", active: [] };
    if (path.endsWith("/environments")) return [{ environmentId: "machine-a", status: "ready", provider: "incus", displayName: "A" }];
    if (method === "PUT" && path.endsWith("/config")) return { ...session, configRevision: 2 };
    return session;
  });
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
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

it("deactivates after the config put, so a put that activates a new default cannot undo the draft", async () => {
  await act(async () => root.render(<QueryClientProvider client={client}>
    <SessionSettingsDialog universeId="universe" sessionId="session" session={session} runActive={false} open onOpenChange={() => {}} />
  </QueryClientProvider>));
  await act(async () => { await new Promise((resolve) => setTimeout(resolve, 20)); });
  const dialog = document.body.querySelector<HTMLElement>('[role="dialog"]')!;

  const model = [...dialog.querySelectorAll<HTMLInputElement>("input")].find((input) => input.value === "saved-model")!;
  const setValue = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
  await act(async () => {
    setValue.call(model, "other-model");
    model.dispatchEvent(new Event("input", { bubbles: true }));
  });
  const panel = dialog.querySelector<HTMLElement>('[aria-label="Enable Environments"]')!.parentElement!.parentElement!;
  await act(async () => panel.querySelector<HTMLButtonElement>("button[aria-expanded]")!.click());
  const active = [...dialog.querySelectorAll<HTMLSelectElement>("select")]
    .find((select) => [...select.options].some((option) => option.textContent === "No active environment"))!;
  await act(async () => {
    active.value = "__no_active_environment__";
    active.dispatchEvent(new Event("change", { bubbles: true }));
  });
  const apply = [...dialog.querySelectorAll<HTMLButtonElement>("button")].find((button) => button.textContent?.includes("Apply setup"))!;
  await act(async () => apply.click());
  await act(async () => { await new Promise((resolve) => setTimeout(resolve, 20)); });

  const writes = mocks.api.mock.calls
    .filter(([method]) => method !== "GET")
    .map(([method, path]) => `${method} ${(path as string).split("/sessions/session")[1]}`);
  expect(writes).toEqual(["PUT /config", "POST /environments/deactivate"]);
});
