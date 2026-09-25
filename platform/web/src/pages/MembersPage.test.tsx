// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { MembersPage } from "./MembersPage";

const mocks = vi.hoisted(() => ({ api: vi.fn(), role: "viewer" }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({
  useActiveUniverse: () => ({ universe: { id: "universe", slug: "test", name: "Test", role: mocks.role }, slug: "test", isLoading: false }),
}));
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
  { id: "member-alice", userId: "alice", name: "Alice", email: "alice@example.test", role: "contributor", createdAt: "" },
  { id: "member-bob", userId: "bob", name: "Bob", email: "bob@example.test", role: "viewer", createdAt: "" },
];
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  mocks.role = "viewer";
  mocks.api.mockReset().mockImplementation(async (_method: string, path: string) => {
    if (path.endsWith("/members")) return members;
    if (path === "/api/v1/users") return [];
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
    <QueryClientProvider client={client}><PermissionIdentityProvider userId="user" platformAdmin={false}><MemoryRouter>{page}</MemoryRouter></PermissionIdentityProvider></QueryClientProvider>,
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

it.each(members)("changes the role of $name in place", async (member) => {
  mocks.role = "admin";
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
  mocks.role = "admin";
  const implementation = mocks.api.getMockImplementation()!;
  mocks.api.mockImplementation(async (method, path, body) => {
    if (method === "PATCH") throw new Error("a universe keeps at least one admin");
    return implementation(method, path, body);
  });
  await show(<MembersPage admin={false} />);
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Edit role for Alice"]')!.click());
  await settle();
  await chooseMemberRole("viewer");
  await act(async () => button("Save role")!.click());
  await settle();
  expect(document.querySelector('[role="alert"]')?.textContent).toContain("at least one admin");
  expect(container.querySelector("tbody tr")?.textContent).toContain("contributor");
  expect(document.querySelector('[role="dialog"]')).not.toBeNull();
});

it("shows other members names and roles without emails or controls", async () => {
  await show(<MembersPage admin={false} />);
  expect([...container.querySelectorAll("thead th")].map((th) => th.textContent)).toEqual(["Name", "Role"]);
  const rows = [...container.querySelectorAll("tbody tr")].map((tr) => [...tr.querySelectorAll("td")].map((td) => td.textContent));
  expect(rows.map(([name]) => name)).toEqual(["Alice", "Bob"]);
  expect(rows[0]![1]).toContain("contributor");
  expect(container.textContent).not.toContain("alice@example.test");
  expect(container.querySelector("button")).toBeNull();
});
