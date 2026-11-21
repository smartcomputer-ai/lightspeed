// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { SessionListPage, SessionOrigin, SessionSummary } from "@/api";
import { MetaToolbar } from "@/components/meta-toolbar";
import { hasLineage, sessionMetaItems, subAgentsTip, useSessionLineage } from "@/components/session/session-meta";

/// The strip under a session header, with only its lineage: the parent and
/// each sub-agent, drawn while there is any.
function SessionLineage({ universeId, handle, sessionId, origin, runRevision, place = "strip" }: {
  universeId: string; handle: string; sessionId: string; origin: SessionOrigin | null; runRevision: number;
  place?: "strip" | "header";
}) {
  const lineage = useSessionLineage({ universeId, sessionId, origin, runRevision });
  if (!hasLineage(lineage)) return null;
  const items = sessionMetaItems({
    session: { id: sessionId, lifecycleStatus: "open", lifecycleControlled: false, activity: "idle", retention: { rootSessionId: sessionId }, origin } as never,
    lineage,
    lineageHref: (id) => `/u/${handle}/sessions/${id}`,
  });
  return <div><MetaToolbar items={items} place={place} /></div>;
}

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({
  ...await original<typeof import("@/api")>(),
  api: mocks.api,
}));

const origin: SessionOrigin = {
  kind: "subagent", parentSessionId: "parent", parentRunId: "parent-run",
  rootSessionId: "parent", depth: 1, invocationId: "invocation",
  agent: { profileId: "reviewer", revision: 1 },
  limits: { maxDepth: 4, maxConcurrent: 5, runTimeoutMs: 60_000 },
};
function child(id = "child", lifecycleStatus: SessionSummary["lifecycleStatus"] = "open"): SessionSummary {
  return {
    id, displayName: `Reviewer ${id}`, lifecycleStatus, createdAtMs: 1, updatedAtMs: 1,
    lifecycleControlled: false, retention: { rootSessionId: id }, access: { visibility: "universe" }, activity: "idle",
  };
}

let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
let requests: { url: string; resolve: (page: SessionListPage) => void; reject: (error: Error) => void }[];

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  requests = [];
  mocks.api.mockReset().mockImplementation((_method: string, url: string) => {
    if (url.endsWith("/sessions/parent")) {
      return Promise.resolve({
        id: "parent", displayName: "Parent session", lifecycleStatus: "open", activity: "working", updatedAtMs: Date.now(),
        origin: { ...origin, agent: { profileId: "planner", revision: 1 } },
      });
    }
    return new Promise<SessionListPage>((resolve, reject) => requests.push({ url, resolve, reject }));
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

async function flush() {
  await act(async () => { await vi.runOnlyPendingTimersAsync(); });
}

async function show(runRevision: number, withParent = false, universeId = "universe", sessionId = "session", place: "strip" | "header" = "strip") {
  await act(async () => root.render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <SessionLineage
          universeId={universeId} handle={universeId} sessionId={sessionId}
          origin={withParent ? origin : null} runRevision={runRevision} place={place}
        />
      </MemoryRouter>
    </QueryClientProvider>,
  ));
  await flush();
}

async function respond(index: number, sessions: SessionSummary[], nextCursor?: string) {
  await act(async () => requests[index]!.resolve({ sessions, nextCursor: nextCursor ?? null }));
  await flush();
}

it.each([false, true])("keeps the lineage mounted during delayed run refreshes (parent: %s)", async (withParent) => {
  await show(0, withParent);
  await respond(0, [child()]);
  const link = container.querySelector('a[href$="/sessions/child"]')!;
  expect(link).not.toBeNull();
  const strip = container.firstElementChild;

  // Production event polling reports acceptance/start separately, with network
  // time between the revision and the refreshed children response.
  for (const revision of [1, 2]) {
    await show(revision, withParent);
    expect(requests).toHaveLength(revision + 1);
    expect(container.querySelectorAll('a[href*="/sessions/"]')).toHaveLength(1);
    expect(container.querySelector('a[href$="/sessions/child"]')).toBe(link);
    expect(container.firstElementChild).toBe(strip);
    // The parent agent opens its details instead of linking straight to it.
    if (withParent) expect(container.querySelector('button[data-meta-item="parent"]')!.getAttribute("aria-label")).toBe("Parent agent: Parent session");
    await respond(revision, [child("child", revision === 2 ? "closed" : "open")]);
    expect(container.querySelector('a[href$="/sessions/child"]')).toBe(link);
  }
  expect(link.querySelector('span.rounded-full[aria-hidden="true"]')?.className).toContain("text-muted-foreground/50");
});

it("retains the header on a refresh error and applies a later successful empty result", async () => {
  await show(0);
  await respond(0, [child()]);
  const link = container.querySelector("a");
  await show(1);
  await act(async () => requests[1]!.reject(new Error("Temporary network failure")));
  await flush();
  expect(container.querySelector("a")).toBe(link);
  expect(container.querySelectorAll('a[href*="/sessions/"]')).toHaveLength(1);

  await show(2);
  await respond(2, []);
  expect(container.textContent).toBe("");
});

it.each(["session", "universe"])("does not carry old lineage into a different %s", async (scope) => {
  await show(0);
  await respond(0, [child()]);
  await show(1);
  await show(1, false, scope === "universe" ? "other" : "universe", scope === "session" ? "other" : "session");
  expect(container.textContent).not.toContain("Reviewer child");
  await respond(1, [child("late-old-child")]);
  expect(container.textContent).not.toContain("late-old-child");
  await respond(2, [child("new-child")]);
  expect(container.textContent).toContain("Reviewer new-child");
});

it("lists the sub-agents in a popover from the header's count, each opening its session", async () => {
  await show(0, false, "universe", "session", "header");
  const working = { ...child("busy"), activity: "working" as const, origin: { ...origin, parentSessionId: "session" } };
  await respond(0, [working, child("done", "closed")], "next");
  const count = container.querySelector<HTMLButtonElement>('button[data-meta-item="subagents"]')!;
  expect(count.getAttribute("aria-label")).toBe("2 sub-agents");

  await act(async () => count.click());
  await flush();
  const list = () => document.body.querySelector('ul[aria-label="Sub-agents"]');
  const rows = [...list()!.querySelectorAll<HTMLAnchorElement>("a")];
  expect(rows.map((row) => row.getAttribute("href"))).toEqual(["/u/universe/sessions/busy", "/u/universe/sessions/done"]);
  expect(rows[0]!.textContent).toContain("Reviewer busy");
  expect(rows[0]!.textContent).toContain("reviewer · Working");
  expect(rows[1]!.textContent).toContain("Closed");

  // More than the first page: the popover offers the next one.
  const more = [...document.body.querySelectorAll("button")].find((button) => button.textContent === "Load more sub-agents")!;
  await act(async () => more.click());
  expect(requests).toHaveLength(2);
  expect(requests[1]!.url).toContain("cursor=next");

  // Opening one closes the popover.
  await act(async () => rows[0]!.click());
  await flush();
  expect(list()).toBeNull();
});

it("names the one sub-agent in the count's tooltip, or the first and how many more", () => {
  expect(subAgentsTip("Audit the scheduler", 1, false)).toBe("Sub-agent: Audit the scheduler");
  expect(subAgentsTip("Audit the scheduler", 3, false)).toBe("Sub-agents: Audit the scheduler and 2 more");
  expect(subAgentsTip("Audit the scheduler", 50, true)).toBe("Sub-agents: Audit the scheduler and 49+ more");
  expect(subAgentsTip(undefined, 2, false)).toBe("2 sub-agents delegated by this session");
});

it("leads with the parent agent, whose popover shows its state and opens it", async () => {
  await show(0, true, "universe", "session", "header");
  await respond(0, []);
  const marks = [...container.querySelectorAll("[data-meta-item]")].map((item) => item.getAttribute("data-meta-item"));
  expect(marks[0]).toBe("parent");
  const parent = container.querySelector<HTMLButtonElement>('button[data-meta-item="parent"]')!;
  expect(parent.getAttribute("aria-label")).toBe("Parent agent: Parent session");

  await act(async () => parent.click());
  await flush();
  const row = document.body.querySelector<HTMLAnchorElement>('ul[aria-label="Parent agent"] a')!;
  expect(row.getAttribute("href")).toBe("/u/universe/sessions/parent");
  expect(row.textContent).toContain("Parent session");
  expect(row.textContent).toContain("planner · Working");
  expect(row.querySelector("[data-tone]")!.getAttribute("data-tone")).toBe("live");
});
