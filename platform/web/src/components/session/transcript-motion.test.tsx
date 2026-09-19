// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { RunSection } from "@/lib/sessions/run-sections";
import { RunSectionView } from "./run-section";
import { TranscriptMotionProvider } from "./transcript-motion";

let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

const run = (key: string): RunSection => ({
  kind: "run", key, runId: key, live: false, work: [],
  input: { kind: "message", key: `${key}-input`, role: "user", text: key },
});

async function render(sections: RunSection[], historyRevision = 0, ready = true) {
  await act(async () => root.render(
    <TranscriptMotionProvider sections={sections} pendingKeys={[]} ready={ready} historyRevision={historyRevision}>
      {sections.map((section) => <RunSectionView key={section.key} section={section}
        activeRun={section.live ? { runId: section.key, label: "thinking", cancelling: false } : null}
        showRunStatistics={false} collapseCompletedRuns />)}
    </TranscriptMotionProvider>,
  ));
}

async function finishEntrance(element: Element, type = "animationend") {
  const event = new Event(type, { bubbles: true });
  Object.defineProperty(event, "animationName", { value: "transcript-enter" });
  await act(async () => element.dispatchEvent(event));
}

it("keeps initial history and older-page arrivals still, then animates new inputs", async () => {
  await render([], 0, false);
  const current = run("current");
  await render([current]);
  expect(container.querySelector(".transcript-enter")).toBeNull();
  await render([run("older"), current], 1);
  expect(container.querySelector(".transcript-enter")).toBeNull();
  await render([run("older"), current, run("new")], 1);
  const entrances = container.querySelectorAll(".transcript-enter");
  expect(entrances).toHaveLength(1);
  expect(entrances[0]!.textContent).toBe("new");
});

it("enters input, activity, and later replies once without replaying confirmation or final promotion", async () => {
  await render([]);
  const section = run("local");
  await render([section]);
  const input = container.querySelector(".transcript-enter")!;
  const confirmed: RunSection = {
    ...section, live: true,
    input: { ...section.input!, key: "backend-input" },
    work: [{ kind: "message", key: "note", role: "assistant", text: "Starting" }],
  };
  await render([confirmed]);
  expect(container.querySelector(".transcript-enter")).toBe(input);
  const activity = container.querySelector('[role="status"]')!.parentElement!;
  expect(activity.classList.contains("transcript-enter")).toBe(true);
  // Contents arriving with the activity box share its entrance.
  expect(activity.querySelector(".transcript-enter")).toBeNull();
  await finishEntrance(activity);

  const reply = { kind: "message" as const, key: "reply", role: "assistant" as const, text: "Answer" };
  await render([{ ...confirmed, work: [...confirmed.work, reply] }]);
  const answer = activity.querySelector(".transcript-enter")!;
  expect(answer.textContent).toBe("Answer");
  const updated = { ...reply, text: "Answer updated" };
  await render([{ ...confirmed, work: [...confirmed.work, updated] }]);
  expect(activity.querySelector(".transcript-enter")).toBe(answer);
  await render([{ ...confirmed, live: false, work: [], reply: updated }]);
  expect(container.querySelector('[data-variant="ghost"]')!.closest(".transcript-enter")).toBeNull();
  expect(container.querySelector(".transcript-enter")).toBe(input);
});

it.each(["animationend", "animationcancel"])("suppresses arrivals throughout the box entrance until %s", async (eventType) => {
  await render([]);
  const section = { ...run("active"), live: true };
  await render([section]);
  const activity = container.querySelector('[role="status"]')!.parentElement!;
  const early = { kind: "message" as const, key: "early", role: "assistant" as const, text: "Early progress" };
  await render([{ ...section, work: [early] }]);
  expect(activity.classList.contains("transcript-enter")).toBe(true);
  expect(activity.querySelector(".transcript-enter")).toBeNull();
  // An unrelated descendant animation must not finish the box's entrance.
  await finishEntrance(activity.querySelector('[data-slot="message"]')!, eventType);
  expect(activity.classList.contains("transcript-enter")).toBe(true);
  await finishEntrance(activity, eventType);
  expect(activity.classList.contains("transcript-enter")).toBe(false);
  expect(activity.querySelector(".transcript-enter")).toBeNull();
  const later = { ...early, key: "later", text: "Later progress" };
  await render([{ ...section, work: [early, later] }]);
  expect(activity.querySelectorAll(".transcript-enter")).toHaveLength(1);
  expect(activity.querySelector(".transcript-enter")!.textContent).toBe("Later progress");
});

it("does not wait for an animation event when reduced motion disables entrances", async () => {
  const preference = { matches: true, addEventListener: vi.fn(), removeEventListener: vi.fn() };
  vi.stubGlobal("matchMedia", vi.fn(() => preference));
  await render([]);
  await render([{ ...run("reduced"), live: true }]);
  expect(container.querySelector(".transcript-enter")).toBeNull();
  expect(preference.removeEventListener).toHaveBeenCalled();
});

it("does not animate previously hidden messages when activity is expanded", async () => {
  await render([]);
  await render([{ ...run("finished"), work: [
    { kind: "message", key: "hidden", role: "assistant", text: "Earlier progress" },
  ] }]);
  const strip = container.querySelector<HTMLButtonElement>('button[aria-label="Show this run\'s activity"]')!;
  const entrances = [...container.querySelectorAll(".transcript-enter")];
  await act(async () => strip.click());
  const message = container.querySelector('[data-variant="ghost"]')!;
  expect(message.textContent).toBe("Earlier progress");
  expect([...container.querySelectorAll(".transcript-enter")]).toEqual(entrances);
  expect(message.closest('[data-slot="message"]')!.parentElement!.classList.contains("transcript-enter")).toBe(false);
});
