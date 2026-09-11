// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { TranscriptLinksContext } from "./tool-trace";
import { TranscriptEntryView, UserBand } from "./transcript-view";

let root: Root;
let container: HTMLDivElement;
let height: number;
let resize: () => void;
const disconnect = vi.fn();

beforeEach(() => {
  height = 40;
  disconnect.mockClear();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("ResizeObserver", class {
    constructor(callback: () => void) { resize = callback; }
    observe() {}
    disconnect = disconnect;
  });
  vi.spyOn(HTMLElement.prototype, "scrollHeight", "get").mockImplementation(() => height);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

it.each([
  ["user:operator", "muted"], ["event", "muted"], ["integration:example", "muted"],
  [undefined, "muted"], ["user:", "muted"],
])("uses the standard muted palette for origin %s without adding a label", async (origin, variant) => {
  await act(async () => root.render(<TranscriptEntryView entry={{ kind: "message", key: "message", role: "user", text: "Hello", origin }} />));
  expect(container.querySelector('[data-slot="bubble"]')?.getAttribute("data-variant")).toBe(variant);
  expect(container.textContent).toBe("Hello");
  expect(container.querySelector("button")).toBeNull();
});

it.each(["user:operator", "event"])("collapses and expands overflowing messages from %s", async (origin) => {
  height = 700;
  const text = "Long input\n".repeat(50) + "Last line";
  await act(async () => root.render(<TranscriptEntryView entry={{ kind: "message", key: "long", role: "user", text, origin, steering: true }} />));
  const button = container.querySelector("button")!;
  const content = document.getElementById(button.getAttribute("aria-controls")!)!;
  expect(button.textContent).toBe("Show more");
  expect(button.getAttribute("aria-expanded")).toBe("false");
  expect(content.style.maxHeight).toBe("160px");
  expect(content.style.maskImage).toContain("linear-gradient");
  expect(content.textContent).toContain("Last line");
  await act(async () => button.click());
  expect(button.textContent).toBe("Show less");
  expect(button.getAttribute("aria-expanded")).toBe("true");
  expect(content.style.maxHeight).toBe("");
  expect(content.style.maskImage).toBe("");
  await act(async () => button.click());
  expect(content.style.maxHeight).toBe("160px");
});

it("rechecks overflow on resize and text changes, and disconnects on unmount", async () => {
  await act(async () => root.render(<UserBand text="A message" />));
  expect(container.querySelector("button")).toBeNull();
  height = 400;
  await act(async () => resize());
  expect(container.querySelector("button")?.textContent).toBe("Show more");
  height = 20;
  await act(async () => root.render(<UserBand text="Shorter" />));
  expect(container.querySelector("button")).toBeNull();
  expect(disconnect).toHaveBeenCalledOnce();
  await act(async () => root.render(null));
  expect(disconnect).toHaveBeenCalledTimes(2);
});

it("uses the standard muted palette for an optimistic message", async () => {
  await act(async () => root.render(<UserBand text="Sending" pending />));
  expect(container.querySelector('[data-slot="bubble"]')?.getAttribute("data-variant")).toBe("muted");
});

it("shows a delivered bot event with its sender, kind and number as a header above the body", async () => {
  const text = "── event #418 · bug.confirmed · bot:support-triage · 14:02\nCursor pagination returns duplicates.\nSecond line.";
  await act(async () => root.render(
    <TranscriptLinksContext.Provider value={{ botName: (id) => ({ "support-triage": "Support Triage" })[id] }}>
      <TranscriptEntryView entry={{ kind: "message", key: "event", role: "user", text, origin: "event" }} />
    </TranscriptLinksContext.Provider>,
  ));
  const content = container.textContent!;
  expect(content).toContain("Support Triage");
  expect(content).toContain("bug.confirmed");
  expect(content).toContain("#418");
  expect(content).toContain("14:02");
  expect(content).toContain("Cursor pagination returns duplicates.\nSecond line.");
  expect(content).not.toContain("── event");
  expect(container.querySelector('[data-slot="bubble"]')?.className).toContain("border-l-2");
});

it("names non-bot event sources by their family and leaves unknown headers alone", async () => {
  const text = "── event #7 · pull_request.opened · webhook:github · Sep 3 09:15\nPR #12 opened.";
  await act(async () => root.render(<TranscriptEntryView entry={{ kind: "message", key: "event", role: "user", text, origin: "event" }} />));
  expect(container.textContent).toContain("github webhook");
  expect(container.textContent).toContain("PR #12 opened.");
  await act(async () => root.render(<TranscriptEntryView entry={{ kind: "message", key: "plain", role: "user", text: "just text", origin: "event" }} />));
  expect(container.textContent).toBe("just text");
});
