// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { SessionListPage, SessionOrigin, SessionSummary } from "@/api";
import { SessionLineage } from "./SessionsPage";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({
  ...await original<typeof import("@/api")>(),
  api: mocks.api,
}));

const origin: SessionOrigin = {
  kind: "subagent", parentSessionId: "parent", parentRunId: "parent-run",
  rootSessionId: "parent", depth: 1, invocationId: "invocation",
  agent: { profileId: "reviewer", revision: 1 },
  limits: { maxDepth: 4, maxDescendants: 20, maxConcurrent: 5, deadlineMs: 60_000 },
};
function child(id = "child", lifecycleStatus: SessionSummary["lifecycleStatus"] = "open"): SessionSummary {
  return {
    id, displayName: `Reviewer ${id}`, lifecycleStatus, createdAtMs: 1, updatedAtMs: 1,
    managed: false, retention: { rootSessionId: id }, access: { visibility: "universe" }, activity: "idle",
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
    if (url.endsWith("/sessions/parent")) return Promise.resolve({ displayName: "Parent session", status: "open" });
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

async function show(runRevision: number, withParent = false, universeId = "universe", sessionId = "session") {
  await act(async () => root.render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <SessionLineage
          universeId={universeId} slug={universeId} sessionId={sessionId}
          origin={withParent ? origin : null} runRevision={runRevision}
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
    expect(container.textContent).toContain("Sub-agents (1)");
    expect(container.querySelector('a[href$="/sessions/child"]')).toBe(link);
    expect(container.firstElementChild).toBe(strip);
    if (withParent) expect(container.textContent).toContain("Parent session");
    await respond(revision, [child("child", revision === 2 ? "closed" : "open")]);
    expect(container.querySelector('a[href$="/sessions/child"]')).toBe(link);
  }
  expect(link.querySelector('[aria-hidden="true"]')?.className).toContain("bg-muted-foreground/50");
});

it("retains the header on a refresh error and applies a later successful empty result", async () => {
  await show(0);
  await respond(0, [child()]);
  const link = container.querySelector("a");
  await show(1);
  await act(async () => requests[1]!.reject(new Error("Temporary network failure")));
  await flush();
  expect(container.querySelector("a")).toBe(link);
  expect(container.textContent).toContain("Sub-agents (1)");

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
