// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from "vitest";

/// A renderer that only counts what it is asked to draw: jsdom has no canvas.
const drawn = vi.hoisted(() => ({ created: 0, renders: 0 }));
vi.mock("./lib/starscape.js", () => ({
  Starscape: class {
    config = { centerX: 0.5, centerY: 0.5 };
    constructor() {
      drawn.created += 1;
    }
    setViewport() { return this; }
    setFade() { return this; }
    render() {
      drawn.renders += 1;
      return this;
    }
  },
}));

import {
  claimDialogStarscape,
  dialogStarscapeRunning,
  keepDialogStarscapeWarm,
  onDialogStarscapeChange,
  PREWARM_DELAY_MS,
  PREWARM_HIDDEN_DROP_MS,
  PREWARM_RESIZE_MS,
  releaseDialogStarscape,
  resetDialogStarscapeForTests,
  runDialogStarscape,
} from "./prewarm";

let hidden = false;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("CanvasRenderingContext2D", class {});
  hidden = false;
  Object.defineProperty(document, "hidden", { configurable: true, get: () => hidden });
  Object.defineProperty(document, "readyState", { configurable: true, get: () => "complete" });
  drawn.created = 0;
  drawn.renders = 0;
  resetDialogStarscapeForTests();
});
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

/// Lets the delay, then the idle moment, pass.
const pass = (ms: number) => vi.advanceTimersByTime(ms + 10);

it("draws one only after the page has loaded, a delay and an idle moment", () => {
  const stop = keepDialogStarscapeWarm();
  expect(drawn.created).toBe(0);
  expect(claimDialogStarscape()).toBeNull();
  pass(PREWARM_DELAY_MS);
  expect(drawn.created).toBe(1);
  expect(drawn.renders).toBe(1);
  stop();
});

it("lends it to one dialog at a time, and takes it back", () => {
  const stop = keepDialogStarscapeWarm();
  pass(PREWARM_DELAY_MS);
  const first = claimDialogStarscape()!;
  expect(first.canvas.dataset.prewarmed).toBe("");
  // While out, another dialog draws its own.
  expect(claimDialogStarscape()).toBeNull();
  releaseDialogStarscape(first);
  expect(claimDialogStarscape()).toBe(first);
  stop();
});

it("draws it again when the window settles at another size, not while a dialog has it", () => {
  const stop = keepDialogStarscapeWarm();
  pass(PREWARM_DELAY_MS);
  const renders = drawn.renders;
  window.innerWidth += 200;
  window.dispatchEvent(new Event("resize"));
  pass(PREWARM_RESIZE_MS);
  expect(drawn.renders).toBe(renders + 1);
  expect(drawn.created).toBe(1);

  const lent = claimDialogStarscape()!;
  window.innerWidth -= 200;
  window.dispatchEvent(new Event("resize"));
  pass(PREWARM_RESIZE_MS);
  expect(drawn.renders).toBe(renders + 1);
  // Back, it is drawn at the new size at the next idle moment.
  releaseDialogStarscape(lent);
  pass(0);
  expect(drawn.renders).toBe(renders + 2);
  stop();
});

it("lets it go while the tab stays hidden, and draws it again on the way back", () => {
  const stop = keepDialogStarscapeWarm();
  pass(PREWARM_DELAY_MS);
  hidden = true;
  document.dispatchEvent(new Event("visibilitychange"));
  pass(PREWARM_HIDDEN_DROP_MS);
  expect(claimDialogStarscape()).toBeNull();
  hidden = false;
  document.dispatchEvent(new Event("visibilitychange"));
  pass(PREWARM_DELAY_MS);
  expect(drawn.created).toBe(2);
  expect(claimDialogStarscape()).not.toBeNull();
  stop();
});

it("lets it go when no longer kept", () => {
  const stop = keepDialogStarscapeWarm();
  pass(PREWARM_DELAY_MS);
  stop();
  expect(claimDialogStarscape()).toBeNull();
  // Nothing is drawn after.
  pass(PREWARM_DELAY_MS);
  expect(drawn.created).toBe(1);
});

it("tells the other starscapes while a dialog's runs, until it stops", () => {
  const changes = vi.fn();
  const unsubscribe = onDialogStarscapeChange(changes);
  expect(dialogStarscapeRunning()).toBe(false);
  const stop = runDialogStarscape();
  expect(dialogStarscapeRunning()).toBe(true);
  stop();
  stop();
  expect(dialogStarscapeRunning()).toBe(false);
  expect(changes).toHaveBeenCalledTimes(2);
  unsubscribe();
});
