// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { idColor } from "@/lib/identity-colors";
import { ThemeProvider } from "@/lib/theme";
import { UiOverviewPage } from "./UiOverviewPage";

let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("matchMedia", (query: string) => ({ matches: false, media: query, addEventListener: () => undefined, removeEventListener: () => undefined }));
  vi.stubGlobal("scrollTo", () => undefined);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

async function showColors() {
  await act(async () => root.render(
    <ThemeProvider>
      <MemoryRouter initialEntries={["/debug/ui-overview/colors"]}>
        <Routes><Route path="/debug/ui-overview/:section" element={<UiOverviewPage />} /></Routes>
      </MemoryRouter>
    </ThemeProvider>,
  ));
}

const swatch = () => container.querySelector<HTMLElement>("[data-id-color-swatch]")!;

it("colours the tester's swatch from the id as it is typed", async () => {
  await showColors();
  const input = container.querySelector<HTMLInputElement>("#id-color-tester")!;
  expect(swatch().style.background).toBe(idColor("my-new-bot"));
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, "release-scribe");
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  expect(swatch().style.background).toBe(idColor("release-scribe"));
});

it("lays out the main surfaces with their tokens", async () => {
  await showColors();
  const text = container.textContent ?? "";
  for (const name of ["Sidebar", "List pane", "Page", "Card"]) expect(text).toContain(name);
  for (const token of ["--sidebar", "--surface-list", "--background", "--surface-card"]) expect(text).toContain(token);
});
