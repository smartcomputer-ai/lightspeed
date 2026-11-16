// @vitest-environment jsdom
import { act, useRef } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { HeaderStarscape } from "@/components/universe-switcher";
import { SETTINGS, settings } from "@/lib/settings";

let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
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
});

function Header() {
  const icon = useRef<HTMLSpanElement>(null);
  return <><span ref={icon} /><HeaderStarscape iconRef={icon} /></>;
}

const show = () => act(async () => root.render(
  <>
    <Header />
    <Dialog open>
      <DialogContent starscape>
        <DialogTitle>New session</DialogTitle>
      </DialogContent>
    </Dialog>
  </>,
));
const starscapes = () => document.querySelectorAll("[data-starscape]").length;

it("shows the sidebar and dialog starscapes unless they are turned off", async () => {
  await show();
  expect(starscapes()).toBe(2);
  await act(async () => settings.set(SETTINGS.starscape, false));
  expect(starscapes()).toBe(0);
  await act(async () => settings.set(SETTINGS.starscape, true));
  expect(starscapes()).toBe(2);
});
