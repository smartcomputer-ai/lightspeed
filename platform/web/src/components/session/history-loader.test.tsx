// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { emptyTranscript } from "@/lib/sessions/transcript";
import type { SessionTail } from "@/lib/sessions/tail";
import { SessionHistoryLoader } from "./history-loader";

let root: Root;
let container: HTMLDivElement;
let viewport: HTMLElement;
let tail: SessionTail;
let height: number;
let top: number;

beforeEach(async () => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => window.setTimeout(() => callback(0), 16));
  vi.stubGlobal("cancelAnimationFrame", (id: number) => window.clearTimeout(id));
  tail = { transcript: emptyTranscript(), phase: "live", error: null, errorIsTransient: false,
    hasOlder: true, loadingOlder: false, historyError: null, historyErrorIsTransient: false, historyRevision: 0,
    loadOlder: vi.fn(), reconcileRuns: vi.fn() };
  height = 900;
  top = 300;
  container = document.createElement("div");
  root = createRoot(container);
  await render();
  viewport = container.querySelector<HTMLElement>('[data-slot="message-scroller-viewport"]')!;
  Object.defineProperties(viewport, {
    scrollHeight: { get: () => height }, clientHeight: { get: () => 600 }, scrollTop: { get: () => top },
  });
  const marker = container.querySelector<HTMLElement>("[data-history-sentinel]")!;
  marker.getBoundingClientRect = () => ({ top: -top, bottom: 1 - top } as DOMRect);
});
afterEach(async () => {
  await act(async () => root.unmount());
  vi.useRealTimers();
  vi.unstubAllGlobals();
});
async function render() {
  await act(async () => root.render(<div data-slot="message-scroller-viewport"><SessionHistoryLoader tail={tail} /></div>));
}
async function scrollTo(position: number) {
  top = position;
  await act(async () => { viewport.dispatchEvent(new Event("scroll")); await vi.advanceTimersByTimeAsync(16); });
}

it("does not load history on open at the bottom, then prefetches when scrolling near the top", async () => {
  await act(async () => vi.advanceTimersByTimeAsync(16));
  expect(tail.loadOlder).not.toHaveBeenCalled();
  await scrollTo(100);
  expect(tail.loadOlder).toHaveBeenCalledOnce();
});

it("fills a short window and stops requesting once history is exhausted", async () => {
  height = 600;
  await scrollTo(0);
  expect(tail.loadOlder).toHaveBeenCalledOnce();
  tail.hasOlder = false;
  await render();
  await scrollTo(0);
  expect(tail.loadOlder).toHaveBeenCalledOnce();
});

it("does not request duplicate pages while history is loading or retrying", async () => {
  tail.loadingOlder = true;
  tail.historyError = "offline";
  tail.historyErrorIsTransient = true;
  await render();
  await scrollTo(0);
  expect(tail.loadOlder).not.toHaveBeenCalled();
  expect(container.textContent).not.toContain("retrying");
  // The scroll helper already advanced one 16 ms animation frame.
  await act(async () => vi.advanceTimersByTime(9_983));
  expect(container.textContent).not.toContain("retrying");
  await act(async () => vi.advanceTimersByTime(1));
  expect(container.textContent).toContain("retrying");
});
