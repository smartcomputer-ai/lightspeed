// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { BotsPage } from "./BotsPage";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({
  useActiveUniverse: () => ({ universe: { id: "universe", role: "viewer" }, handle: "universe", isLoading: false }),
  useFeature: () => true,
}));
// The list is under test; the bot's page beside it only needs to be there.
vi.mock("@/components/bot/detail", () => ({ BotDetail: () => <div>Bot page</div> }));

let root: Root;
let container: HTMLDivElement;
let client: QueryClient;

const bot = (botId: string, openSessionCount?: number) => ({
  botId, displayName: botId, profileId: "shared", revision: 1, eventSeq: 0, selfConfig: true, emit: false, enabled: true,
  createdAtMs: 0, updatedAtMs: 0, triggerCount: 0, pendingCount: 0, lastEvent: null, activity: "idle",
  access: { visibility: "universe" }, ...(openSessionCount === undefined ? {} : { openSessionCount }),
});
const summary = (id: string, parent?: string) => ({
  id, displayName: `Sub ${id}`, lifecycleStatus: "open", activity: "idle", createdAtMs: 1, updatedAtMs: 1, lifecycleControlled: true,
  retention: { rootSessionId: "main" }, access: { visibility: "universe" },
  ...(parent ? {
    origin: {
      kind: "subagent", parentSessionId: parent, parentRunId: "run", rootSessionId: "main", depth: 1, invocationId: "inv",
      agent: { profileId: "reviewer", revision: 1 }, limits: { maxDepth: 2, maxConcurrent: 1, runTimeoutMs: 1 },
    },
  } : {}),
});
/// Alpha: Main with a sub-agent of its own, and a thread with a sub-agent.
const alphaState = {
  controller: {
    mainSessionId: "alpha-main", controllerStatus: "idle", setupStatus: "ready", enabled: true, closed: false, activeDeliveries: [],
    sessions: [
      { sessionId: "alpha-main", label: "main", kind: "main", busy: false, generation: 1 },
      { sessionId: "thread", label: "PR-1", kind: "perKey", busy: false, generation: 1, lastActiveAtMs: 1 },
    ],
  },
  sessions: [summary("main-sub", "alpha-main"), summary("thread-sub", "thread")],
};
const betaState = {
  controller: {
    mainSessionId: "beta-main", controllerStatus: "idle", setupStatus: "ready", enabled: true, closed: false, activeDeliveries: [],
    sessions: [
      { sessionId: "beta-main", label: "main", kind: "main", busy: false, generation: 1 },
      { sessionId: "beta-thread", label: "Digest", kind: "oneShot", busy: false, generation: 1 },
    ],
  },
  sessions: [],
};

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mocks.api.mockReset().mockImplementation(async (_method: string, path: string) => {
    if (path.endsWith("/bots")) return { bots: [bot("alpha", 3), bot("beta", 2), bot("gamma", 1), bot("delta")] };
    if (path.endsWith("/bots/alpha/state")) return { state: alphaState };
    if (path.endsWith("/bots/beta/state")) return { state: betaState };
    if (/\/bots\/\w+$/.test(path)) return { bot: bot(path.split("/").at(-1)!, 1) };
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

async function show(path: string) {
  await act(async () => root.render(
    <QueryClientProvider client={client}><PermissionIdentityProvider userId="user" platformAdmin={false}><MemoryRouter initialEntries={[path]}><Routes>
      <Route path="/u/:handle/bots/:botId" element={<BotsPage />} />
      <Route path="/u/:handle/bots/:botId/chat/:sessionId" element={<BotsPage />} />
    </Routes></MemoryRouter></PermissionIdentityProvider></QueryClientProvider>,
  ));
  for (let i = 0; i < 3; i++) await act(async () => { await vi.advanceTimersByTimeAsync(10); });
}

const titles = () => [...container.querySelectorAll("li a")].map((link) => link.getAttribute("href")!.split("/").slice(4).join("/"));
const caret = (name: string) => [...container.querySelectorAll<HTMLButtonElement>('[data-slot="list-row-caret"]')]
  .find((button) => button.getAttribute("aria-label")?.includes(name));
const stateReads = () => mocks.api.mock.calls.map(([, path]) => path as string).filter((path) => path.endsWith("/state"));

it("opens the selected bot one level down and reads no other bot's conversations", async () => {
  await show("/u/universe/bots/alpha");
  // Alpha's thread and Main's sub-agent show; the thread's sub-agent waits behind its caret.
  expect(titles()).toEqual(["alpha", "alpha/chat/thread", "alpha/chat/main-sub", "beta", "delta", "gamma"]);
  expect(caret("alpha's")!.getAttribute("aria-expanded")).toBe("true");
  expect(caret("PR-1")!.getAttribute("aria-expanded")).toBe("false");
  // Beta's count says it has more than Main; gamma's says it has only Main;
  // delta's is not known, so it offers nothing to open.
  expect(caret("beta's")!.getAttribute("aria-expanded")).toBe("false");
  expect(caret("gamma's")).toBeUndefined();
  expect(caret("delta's")).toBeUndefined();
  expect(new Set(stateReads())).toEqual(new Set(["/api/ui/universes/universe/bots/alpha/state"]));
});

it("reads another bot's conversations only once its row is opened", async () => {
  await show("/u/universe/bots/alpha");
  await act(async () => caret("beta's")!.click());
  for (let i = 0; i < 3; i++) await act(async () => { await vi.advanceTimersByTimeAsync(10); });
  expect(stateReads()).toContain("/api/ui/universes/universe/bots/beta/state");
  expect(titles()).toContain("beta/chat/beta-thread");

  // Closed again, it is no longer refreshed.
  await act(async () => caret("beta's")!.click());
  const reads = stateReads().filter((path) => path.includes("/beta/")).length;
  await act(async () => { await vi.advanceTimersByTimeAsync(60_000); });
  expect(stateReads().filter((path) => path.includes("/beta/")).length).toBe(reads);
  expect(titles()).not.toContain("beta/chat/beta-thread");
});

it("opens the rows above a selected conversation, and lets them be closed", async () => {
  await show("/u/universe/bots/alpha/chat/thread-sub");
  expect(titles()).toContain("alpha/chat/thread-sub");
  expect(container.querySelector('a[href$="/chat/thread-sub"]')!.getAttribute("data-highlighted")).toBe("true");
  await act(async () => caret("PR-1")!.click());
  expect(titles()).not.toContain("alpha/chat/thread-sub");
});
