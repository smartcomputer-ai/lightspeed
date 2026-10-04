// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { type SessionView, api } from "@/api";
import { useSessionCompaction } from "./compaction";

vi.mock("@/api", () => ({ api: vi.fn() }));
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
let invalidate: ReturnType<typeof vi.spyOn>;
let result: ReturnType<typeof useSessionCompaction>;
let observer: ReturnType<typeof useSessionCompaction>;

function Controls({ sessionId, session }: { sessionId: string; session?: SessionView }) {
  result = useSessionCompaction("universe", sessionId, session);
  observer = useSessionCompaction("universe", sessionId, session);
  return null;
}
async function show(sessionId = "session", session?: SessionView) {
  await act(async () => root.render(<QueryClientProvider client={client}><Controls sessionId={sessionId} session={session} /></QueryClientProvider>));
  await flush();
}
async function flush() {
  await act(async () => { await vi.advanceTimersByTimeAsync(10); });
}
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.mocked(api).mockReset();
  client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  invalidate = vi.spyOn(client, "invalidateQueries");
  container = document.createElement("div");
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  client.clear();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

it("shares pending state between the menu and transcript, prevents duplicates, and refreshes after completion", async () => {
  let finish!: () => void;
  vi.mocked(api).mockImplementation(() => new Promise<void>((resolve) => { finish = resolve; }));
  await show("bot/session");
  await act(async () => result.compact());
  await flush();
  expect(api).toHaveBeenCalledWith("POST", "/api/v1/universes/universe/sessions/bot%2Fsession/context/compact");
  expect(observer.label).toBe("Requesting compaction…");
  await act(async () => observer.compact());
  expect(api).toHaveBeenCalledTimes(1);
  await act(async () => finish());
  await flush();
  expect(observer.label).toBeNull();
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["session", "universe", "bot/session"] });
});

it.each([
  [true, false, "Compacting context…"],
  [false, true, "Context compaction queued…"],
] as const)("restores server progress pending=%s queued=%s", async (pending, queued, label) => {
  await show("session", { activeContext: { compaction: { pending, queued } } } as SessionView);
  expect(observer.label).toBe(label);
  await act(async () => result.compact());
  expect(api).not.toHaveBeenCalled();
});

it("shows failures to both consumers and clears them on retry", async () => {
  vi.mocked(api).mockRejectedValueOnce(new Error("No compactable context"));
  await show();
  await act(async () => result.compact());
  await flush();
  expect(observer.error).toBe("No compactable context");
  expect(observer.label).toBeNull();
  expect(invalidate).toHaveBeenCalled();
  vi.mocked(api).mockResolvedValueOnce({});
  await act(async () => result.compact());
  await flush();
  expect(observer.error).toBeNull();
});

it("keeps an outstanding request scoped to its original session after navigation", async () => {
  let finish!: () => void;
  vi.mocked(api).mockImplementation(() => new Promise<void>((resolve) => { finish = resolve; }));
  await show("first");
  await act(async () => result.compact());
  await flush();
  await show("second");
  expect(observer.label).toBeNull();
  await act(async () => finish());
  await flush();
  expect(observer.label).toBeNull();
  expect(invalidate).toHaveBeenCalledWith({ queryKey: ["session", "universe", "first"] });
});
