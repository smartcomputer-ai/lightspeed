// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { enableDemoMode } from "@/lib/demo-mode";
import { ThemeProvider } from "@/lib/theme";
import { PreferencesPage } from "./PreferencesPage";

let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("matchMedia", (query: string) => ({ matches: false, media: query, addEventListener: () => undefined, removeEventListener: () => undefined }));
  localStorage.clear();
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
});

const show = () => act(async () => root.render(<ThemeProvider><PreferencesPage /></ThemeProvider>));
const button = (label: string) => [...container.querySelectorAll<HTMLButtonElement>("button")].find((node) => node.textContent?.trim() === label);

it("remembers the chosen theme and applies it", async () => {
  await show();
  await act(async () => button("Dark")!.click());
  expect(localStorage.getItem("lightspeed:theme")).toBe("dark");
  expect(document.documentElement.classList.contains("dark")).toBe(true);
  await act(async () => button("Light")!.click());
  expect(localStorage.getItem("lightspeed:theme")).toBe("light");
  expect(document.documentElement.classList.contains("dark")).toBe(false);
});

// Demo mode stays on once enabled, so this order matters: off, then on.
it("leaves the demo data out of a real deployment", async () => {
  await show();
  expect(container.querySelector("#demo-data")).toBeNull();
});

it("says what the demo is above the reset, and resets it", async () => {
  const reset = vi.fn();
  enableDemoMode({ reset });
  await show();
  const card = container.querySelector("#demo-data")!;
  const note = card.querySelector('[data-slot="info-box"]')!;
  expect(note.textContent).toContain("Demo Mode");
  expect(note.textContent).toContain("runs right in your browser on example data");
  // The card is where the reset is, so the note does not point to it.
  expect(note.textContent).not.toContain("Want a fresh start?");
  await act(async () => button("Reset demo data")!.click());
  expect(reset).toHaveBeenCalledOnce();
});
