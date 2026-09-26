// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { McpServersPage, mcpConnectionSummary } from "./McpServersPage";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({ useActiveUniverse: () => ({ universe: { id: "universe", role: "operator", slug: "test", name: "Test" }, slug: "test", isLoading: false }) }));
// Every select becomes a native control named by its trigger; jsdom cannot lay out the popup.
vi.mock("@/components/ui/select", async () => {
  const React = await import("react");
  function SelectTrigger(_props: { "aria-label"?: string; children?: ReactNode }) {
    return null;
  }
  function Select({ value, onValueChange, children }: { value: string; onValueChange: (value: string) => void; children: ReactNode }) {
    const options: { value: string; label: ReactNode }[] = [];
    let label: string | undefined;
    const visit = (nodes: ReactNode) =>
      React.Children.forEach(nodes, (child) => {
        if (!React.isValidElement(child)) return;
        const props = child.props as { value?: string; children?: ReactNode; "aria-label"?: string };
        if (child.type === SelectTrigger) label = props["aria-label"];
        else if (props.value !== undefined) options.push({ value: props.value, label: props.children });
        else if (props.children) visit(props.children);
      });
    visit(children);
    return (
      <select aria-label={label} value={value} onChange={(event) => onValueChange(event.target.value)}>
        {options.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
      </select>
    );
  }
  const Part = () => null;
  return { Select, SelectTrigger, SelectContent: ({ children }: { children?: ReactNode }) => <>{children}</>, SelectItem: Part, SelectValue: Part };
});
const saved = {
  serverId: "tools", displayName: "Research tools", serverUrl: "https://example.test/mcp", defaultServerLabel: "tools",
  execution: "native", exposure: "search", approval: "never", allowPrivateNetwork: false, revision: 4,
  authPolicy: { type: "none" }, status: "disabled", allowedTools: null,
};
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  mocks.api.mockReset().mockImplementation(async (method: string, path: string) => {
    if (method === "GET" && path.endsWith("/mcp-servers")) return [saved];
    if (path.endsWith("/auth-grants")) return [];
    if (path.endsWith("/discover-auth")) return { oauth: null };
    if (path.endsWith("/tools/discover")) return { status: "success", tools: [] };
    if (method === "POST" && path.endsWith("/mcp-servers")) return { ...saved, serverId: "github" };
    if (method === "PUT") return saved;
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
  vi.useRealTimers();
  vi.unstubAllGlobals();
});
async function settle() {
  for (let step = 0; step < 5; step++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });
}
async function show() {
  await act(async () => root.render(
    <QueryClientProvider client={client}><PermissionIdentityProvider userId="user" platformAdmin={false}><MemoryRouter>
      <McpServersPage admin />
    </MemoryRouter></PermissionIdentityProvider></QueryClientProvider>,
  ));
  await settle();
}
const dialog = () => document.body.querySelector<HTMLElement>('[role="dialog"]')!;
const field = (selector: string) => dialog().querySelector(selector);
const button = (text: string) =>
  [...document.body.querySelectorAll<HTMLButtonElement>("button")].find((item) => item.textContent?.trim() === text);
async function click(target: HTMLElement | null | undefined) {
  expect(target).toBeTruthy();
  await act(async () => target!.click());
  await settle();
}
async function type(selector: string, value: string) {
  const input = dialog().querySelector<HTMLInputElement>(selector)!;
  expect(input).not.toBeNull();
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
async function choose(label: string, value: string) {
  const select = dialog().querySelector<HTMLSelectElement>(`select[aria-label="${label}"]`)!;
  expect(select).not.toBeNull();
  await act(async () => {
    select.value = value;
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
}
const summary = () =>
  [...dialog().querySelectorAll('[data-slot="settings-disclosure"]')].at(-1)!.querySelector("p")!.textContent;

it("names the server first, then confirms the connection with everything else behind one summary", async () => {
  await show();
  await click(button("Add server"));
  // Step 1: name, URL and description; the id is derived and changed on demand.
  expect(field("#mcp-name")).not.toBeNull();
  expect(field("#mcp-url")).not.toBeNull();
  expect(field("#mcp-description")).not.toBeNull();
  expect(field('select[aria-label="Authentication"]')).toBeNull();
  await type("#mcp-name", "GitHub");
  expect(dialog().textContent).toContain("Profiles reference it as github·Change");
  expect(field("#mcp-id")).toBeNull();
  await click(field('[aria-label="Change server ID"]') as HTMLButtonElement);
  expect((field("#mcp-id") as HTMLInputElement).value).toBe("github");
  await type("#mcp-url", "https://mcp.example.com/mcp");
  await type("#mcp-description", "Issues and pull requests");
  await click(button("Continue"));

  // Step 2: authentication in the open, the rest summarized with its defaults.
  expect(field('select[aria-label="Authentication"]')).not.toBeNull();
  expect(field('select[aria-label="Tool approval"]')).toBeNull();
  expect(summary()).toBe("Model provider connects · no approval·Customize");
  await click(field('[aria-label="Customize connection options"]') as HTMLButtonElement);
  const headings = [...dialog().querySelectorAll('[role="group"][aria-labelledby]')].map(
    (group) => document.getElementById(group.getAttribute("aria-labelledby")!)?.textContent,
  );
  expect(headings).toEqual(["Tools", "Network"]);
  await choose("MCP execution", "native");
  await choose("MCP tool exposure", "search");
  await choose("Tool approval", "always");
  expect(summary()).toBe(
    "Lightspeed connects · tools searched on demand · approval: always ask·Hide",
  );
  // Choosing provider execution resets exposure, and the summary follows.
  await choose("MCP execution", "provider");
  expect(summary()).toContain("Model provider connects · approval: always ask");
  await choose("MCP execution", "native");
  const submit = dialog().querySelector<HTMLButtonElement>('button[type="submit"]')!;
  expect(submit.textContent).toBe("Add server");
  await click(submit);
  const create = mocks.api.mock.calls.find(([method, path]) => method === "POST" && (path as string).endsWith("/mcp-servers"))?.[2];
  expect(create).toMatchObject({
    serverId: "github",
    displayName: "GitHub",
    description: "Issues and pull requests",
    execution: "native",
    exposure: "inject",
    approval: "always",
  });
  expect(create).not.toHaveProperty("access");
});

it("edits on one page with the status switch as the first row", async () => {
  await show();
  await click(container.querySelector<HTMLButtonElement>('[aria-label="Edit tools"]'));
  expect(dialog().querySelector('[aria-label="MCP server creation progress"]')).toBeNull();
  const scroller = dialog().querySelector("form")!.firstElementChild!;
  const first = scroller.firstElementChild!;
  expect(first.textContent).toContain("Research tools");
  expect(first.textContent).toContain("Disabled. It stays configured");
  const enabled = first.querySelector<HTMLElement>('[role="switch"][aria-label="Enabled"]')!;
  expect(enabled).not.toBeNull();
  // Server and connection fields follow on the same page.
  expect(field("#mcp-name")).not.toBeNull();
  expect(field("#mcp-description")).not.toBeNull();
  expect(field("#mcp-id")).toBeNull();
  expect(field('select[aria-label="Authentication"]')).not.toBeNull();
  expect(summary()).toBe("Lightspeed connects · tools searched on demand · no approval·Customize");

  await click(enabled);
  expect(first.textContent).toContain("Enabled. Profiles and sessions can link it.");
  await click(button("Save"));
  const put = mocks.api.mock.calls.find(([method]) => method === "PUT")?.[2];
  expect(put).toMatchObject({ serverId: "tools", revision: 4, status: "active", exposure: "search" });
});

it("summarizes OAuth details and private-network access only when they differ", () => {
  expect(mcpConnectionSummary({
    execution: "provider", exposure: "inject", approval: "never", allowPrivateNetwork: true, customOAuthSettings: 2,
  })).toBe("Model provider connects · no approval · private network allowed · 2 custom OAuth settings");
});
