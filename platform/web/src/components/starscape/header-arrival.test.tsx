// @vitest-environment jsdom
import { act, useRef } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { HEADER_STARSCAPE_FADE_OUT_MS, HEADER_STARSCAPE_HOLD_MS, HeaderStarscape } from "@/components/universe-switcher";
import { SETTINGS, settings } from "@/lib/settings";

let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  localStorage.clear();
  document.documentElement.classList.add("dark");
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  localStorage.clear();
  document.documentElement.classList.remove("dark");
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

function Header({ arrival }: { arrival?: string | null }) {
  const icon = useRef<HTMLSpanElement>(null);
  return <><span ref={icon} /><HeaderStarscape iconRef={icon} arrival={arrival} /></>;
}
const render = (arrival?: string | null) => act(async () => root.render(<Header arrival={arrival} />));
const sky = () => container.querySelector<HTMLElement>("[data-starscape]");
const advance = (ms: number) => act(async () => { vi.advanceTimersByTime(ms); });

it("shows on arrival, fades after the hold, and is removed once faded", async () => {
  await render("arrival-one");
  expect(sky()).not.toBeNull();
  expect(sky()!.classList.contains("opacity-0")).toBe(false);
  await advance(HEADER_STARSCAPE_HOLD_MS);
  expect(sky()!.classList.contains("opacity-0")).toBe(true);
  await advance(HEADER_STARSCAPE_FADE_OUT_MS);
  expect(sky()).toBeNull();
});

it("starts over at full brightness on a switch, even mid-fade", async () => {
  await render("arrival-two");
  await advance(HEADER_STARSCAPE_HOLD_MS + 100);
  expect(sky()!.classList.contains("opacity-0")).toBe(true);
  await render("arrival-three");
  expect(sky()!.classList.contains("opacity-0")).toBe(false);
  await advance(HEADER_STARSCAPE_HOLD_MS + HEADER_STARSCAPE_FADE_OUT_MS);
  expect(sky()).toBeNull();
});

it("does not replay for the same universe when the header mounts again", async () => {
  await render("arrival-four");
  await advance(HEADER_STARSCAPE_HOLD_MS + HEADER_STARSCAPE_FADE_OUT_MS);
  await act(async () => root.unmount());
  root = createRoot(container);
  await render("arrival-four");
  expect(sky()).toBeNull();
});

it("shows nothing without a universe, or with reduced motion", async () => {
  await render(null);
  expect(sky()).toBeNull();
  settings.set(SETTINGS.reducedMotion, "on");
  await render("arrival-five");
  expect(sky()).toBeNull();
});

it("stays in previews, which name no arrival", async () => {
  await render(undefined);
  await advance(HEADER_STARSCAPE_HOLD_MS + HEADER_STARSCAPE_FADE_OUT_MS);
  expect(sky()).not.toBeNull();
});
