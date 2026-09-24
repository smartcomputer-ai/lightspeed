// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { UniverseAction } from "@lightspeed-ai/agent-client";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { MembersPage } from "./MembersPage";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({ useActiveUniverse: () => ({ universe: { id: "universe", role: "admin", slug: "test", name: "Test" }, slug: "test", isLoading: false }) }));
// JSDOM has no popup layout; keep the real options and exercise changes with a native select.
vi.mock("@/components/ui/select", async () => {
  const React = await import("react");
  const Part = () => null;
  return {
    Select: ({ value, onValueChange, children, disabled }: { value: string; onValueChange: (value: string) => void; children: React.ReactNode; disabled?: boolean }) => {
      const options: { value: string; label: React.ReactNode }[] = [];
      const visit = (children: React.ReactNode) => React.Children.forEach(children, (child) => {
        if (!React.isValidElement(child)) return;
        const props = child.props as { value?: string; children?: React.ReactNode };
        if (props.value !== undefined) options.push({ value: props.value, label: props.children });
        else if (props.children) visit(props.children);
      });
      visit(children);
      return <select aria-label="Role" value={value} disabled={disabled} onChange={(event) => onValueChange(event.target.value)}>
        {options.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
      </select>;
    },
    SelectContent: Part, SelectItem: Part, SelectTrigger: Part, SelectValue: Part,
  };
});

const members = [
  { id: "principal:alice:contributor", userId: "alice", name: "Alice", email: "alice@example.test", role: "contributor", createdAt: "" },
  { id: "group:team:viewer", userId: "team", name: "Team", email: "Group", role: "viewer", createdAt: "" },
];
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
let actions: UniverseAction[];
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  actions = ["read"];
  mocks.api.mockReset().mockImplementation(async (_method: string, path: string) => {
    if (path.endsWith("/access")) return { actions, resources: [] };
    if (path.endsWith("/members")) return members;
    if (path.endsWith("/groups") || path === "/api/v1/users") return [];
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
  for (let step = 0; step < 5; step++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });
}
async function show(page: React.ReactNode) {
  await act(async () => root.render(
    <QueryClientProvider client={client}><PermissionIdentityProvider userId="user"><MemoryRouter>{page}</MemoryRouter></PermissionIdentityProvider></QueryClientProvider>,
  ));
  await settle();
}
function button(label: string) {
  return [...document.body.querySelectorAll<HTMLButtonElement>("button")].find((node) => node.textContent?.trim() === label);
}
async function chooseMemberRole(role: string) {
  const select = document.querySelector<HTMLSelectElement>('[role="dialog"] select[aria-label="Role"]')!;
  await act(async () => {
    select.value = role;
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await settle();
}

it.each(members)("edits the specific role grant for $name", async (member) => {
  actions.push("manage_access");
  const implementation = mocks.api.getMockImplementation()!;
  mocks.api.mockImplementation(async (method, path, body) => {
    if (method === "PATCH") return { changed: true };
    return implementation(method, path, body);
  });
  await show(<MembersPage admin={false} />);
  await act(async () => container.querySelector<HTMLButtonElement>(`[aria-label="Edit role for ${member.name}"]`)!.click());
  await settle();
  expect(button("Save role")!.disabled).toBe(true);
  await chooseMemberRole("operator");
  await act(async () => button("Save role")!.click());
  await settle();
  expect(mocks.api).toHaveBeenCalledWith("PATCH", `/api/v1/universes/universe/members/${member.id}`, { role: "operator" });
  expect(mocks.api.mock.calls.some(([method, path]) => path.includes("/members") && (method === "DELETE" || method === "POST"))).toBe(false);
  expect(document.querySelector('[role="dialog"]')).toBeNull();
});

it("keeps the original member role and explains a rejected edit", async () => {
  actions.push("manage_access");
  const implementation = mocks.api.getMockImplementation()!;
  mocks.api.mockImplementation(async (method, path, body) => {
    if (method === "PATCH") throw new Error("At least one active administrator must remain.");
    return implementation(method, path, body);
  });
  await show(<MembersPage admin={false} />);
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Edit role for Alice"]')!.click());
  await settle();
  await chooseMemberRole("viewer");
  await act(async () => button("Save role")!.click());
  await settle();
  expect(document.querySelector('[role="alert"]')?.textContent).toContain("At least one active administrator");
  expect(container.querySelector("tbody tr")?.textContent).toContain("contributor");
  expect(document.querySelector('[role="dialog"]')).not.toBeNull();
  actions = ["read"];
  await act(async () => { await client.invalidateQueries({ queryKey: ["action-permissions"] }); });
  await settle();
  expect(document.querySelector('[role="dialog"]')).toBeNull();
});

it("shows the agent identity read-only", async () => {
  actions.push("manage_access");
  const agent = { id: "principal:agent:executor", userId: "agent", name: "Default agent identity", email: "Agent identity", role: "executor", system: true, createdAt: "" };
  const implementation = mocks.api.getMockImplementation()!;
  mocks.api.mockImplementation(async (method, path, body) =>
    path.endsWith("/members") ? [...members, agent] : implementation(method, path, body));
  await show(<MembersPage admin={false} />);
  const row = [...container.querySelectorAll("tbody tr")].find((tr) => tr.textContent?.includes("Default agent identity"))!;
  expect(row.textContent).toContain("Executor");
  expect(row.querySelector("a")?.getAttribute("href")).toBe("/u/test/settings/general");
  expect(row.querySelector("button")).toBeNull();
  expect(container.querySelector('[aria-label="Edit role for Alice"]')).not.toBeNull();
});

it("shows other members names, kinds and roles without emails or controls", async () => {
  const implementation = mocks.api.getMockImplementation()!;
  mocks.api.mockImplementation(async (method, path, body) => path.endsWith("/members") ? [
    // A stray admin-only field never reaches a reader's screen.
    { id: "principal:alice:contributor", subject: { kind: "principal", id: "alice" }, principalKind: "user", name: "Alice", role: "contributor", readPrivateContent: true, email: "alice@example.test" },
    { id: "group:team:viewer", subject: { kind: "group", id: "team" }, name: "Team", role: "viewer" },
    { id: "principal:svc:operator", subject: { kind: "principal", id: "svc" }, principalKind: "service", name: "Configurator", role: "operator" },
    { id: "principal:agent:executor", subject: { kind: "principal", id: "agent" }, principalKind: "service", name: "Default agent identity", role: "executor", system: true },
  ] : implementation(method, path, body));
  await show(<MembersPage admin={false} />);
  expect([...container.querySelectorAll("thead th")].map((th) => th.textContent)).toEqual(["Name", "Kind", "Role"]);
  const rows = [...container.querySelectorAll("tbody tr")].map((tr) => [...tr.querySelectorAll("td")].map((td) => td.textContent));
  expect(rows.map(([name, kind]) => [name, kind])).toEqual([
    ["Alice", "Person"], ["Team", "Group"], ["Configurator", "Service"], ["Default agent identity", "Agent identity"],
  ]);
  expect(container.textContent).not.toContain("alice@example.test");
  expect(container.textContent).not.toContain("Private-content access");
  expect(container.querySelector("tbody a")).toBeNull();
  expect(container.querySelector("button")).toBeNull();
});
