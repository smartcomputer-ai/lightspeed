// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { SessionSummary } from "@/api";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { buildSessionTree, SessionsPage, useOpenSessionRows } from "./SessionsPage";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({ useActiveUniverse: () => ({ universe: { id: "universe", role: "viewer" }, handle: "universe", isLoading: false }) }));
vi.mock("@/components/provider-readiness-banner", () => ({ ProviderReadinessBanner: () => null }));

let root: Root;
let container: HTMLDivElement;
let client: QueryClient;

/// A root session with a sub-agent, which has one of its own, and another root.
const session = (id: string, parent?: string): SessionSummary => ({
  id, displayName: id, lifecycleStatus: "open", lifecycleControlled: false, createdAtMs: 0, updatedAtMs: 0, activity: "idle",
  retention: { rootSessionId: "root" }, access: { visibility: "universe" },
  ...(parent ? {
    origin: {
      kind: "subagent", parentSessionId: parent, parentRunId: "run", rootSessionId: "root", depth: 1, invocationId: "inv",
      agent: { profileId: "reviewer", revision: 1 }, limits: { maxDepth: 3, maxConcurrent: 1, runTimeoutMs: 1 },
    },
  } : {}),
});
const sessions = [session("root"), session("child", "root"), session("grandchild", "child"), session("other")];

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  window.localStorage.clear();
  mocks.api.mockReset().mockImplementation(async (_method: string, path: string) => {
    if (path.includes("/sessions?")) return { sessions };
    if (path.includes("/environments")) return { environments: [], nextAfter: null };
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

const rows = () => [...container.querySelectorAll("li a")].map((link) => link.getAttribute("href")!.split("/").at(-1));
const caret = (label: string) => [...container.querySelectorAll<HTMLButtonElement>('[data-slot="list-row-caret"]')]
  .filter((button) => button.getAttribute("aria-label") === label);

it("lists each session's sub-agents behind a caret after its title, one level at a time", async () => {
  await act(async () => root.render(
    <QueryClientProvider client={client}><PermissionIdentityProvider userId="user" platformAdmin={false}><MemoryRouter>
      <SessionsPage />
    </MemoryRouter></PermissionIdentityProvider></QueryClientProvider>,
  ));
  for (let i = 0; i < 3; i++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });
  expect(container.querySelector('[data-list-expand="title"]')).not.toBeNull();
  // Nothing is selected: every session starts closed.
  expect(rows()).toEqual(["root", "other"]);
  await act(async () => caret("Show 1 sub-agent")[0]!.click());
  expect(rows()).toEqual(["root", "child", "other"]);
  // The sub-agent has its own caret; its sub-agent waits behind it.
  expect(caret("Show 1 sub-agent")).toHaveLength(1);
  await act(async () => caret("Show 1 sub-agent")[0]!.click());
  expect(rows()).toEqual(["root", "child", "grandchild", "other"]);
  await act(async () => caret("Hide 1 sub-agent")[0]!.click());
  expect(rows()).toEqual(["root", "other"]);
});

/// Shows which sessions the hook opens for a selection.
function Opened({ activeId, loaded }: { activeId: string | undefined; loaded: SessionSummary[] }) {
  const open = useOpenSessionRows(buildSessionTree(loaded), activeId);
  return <p>{loaded.filter((item) => open.isOpen(item.id)).map((item) => item.id).join(",")}</p>;
}

it("opens the selected session one level down and the rows above it, once it is loaded", async () => {
  const render = (activeId: string | undefined, loaded: SessionSummary[]) =>
    act(async () => root.render(<Opened activeId={activeId} loaded={loaded} />));
  await render(undefined, sessions);
  expect(container.textContent).toBe("");
  // A root: itself, so its sub-agents show; theirs stay closed.
  await render("root", sessions);
  expect(container.textContent).toBe("root");
  // Deeper, before it is loaded: nothing yet; once loaded, the path to it.
  await render("grandchild", sessions.slice(0, 2));
  expect(container.textContent).toBe("root");
  await render("grandchild", sessions);
  expect(container.textContent).toBe("root,child,grandchild");
});
