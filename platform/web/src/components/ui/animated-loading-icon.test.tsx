// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { AnimatedLoadingIcon } from "./animated-loading-icon";

it("spins while it shows and while it fades out, then rests hidden", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const container = document.createElement("div");
  const root = createRoot(container);
  const show = (active: boolean) => act(async () => root.render(<AnimatedLoadingIcon active={active} />));
  const host = () => container.querySelector<HTMLElement>('[data-slot="animated-loading-icon"]')!;
  const spinning = () => host().querySelector("svg")!.getAttribute("class")?.includes("animate-spin") ?? false;

  await show(false);
  expect(host().getAttribute("class")).toContain("opacity-0");
  expect(spinning()).toBe(false);

  // Begin: shown full size, growing in from a fifth on its first frame.
  await show(true);
  expect(host().hasAttribute("data-active")).toBe(true);
  expect(host().getAttribute("class")).toContain("scale-100");
  expect(host().getAttribute("class")).toContain("starting:scale-20");
  expect(spinning()).toBe(true);

  // Stop: it fades and shrinks, still spinning until the fade ends.
  await show(false);
  expect(host().getAttribute("class")).toContain("scale-20");
  expect(spinning()).toBe(true);
  await act(async () => host().dispatchEvent(new Event("transitionend", { bubbles: true })));
  expect(spinning()).toBe(false);

  await act(async () => root.unmount());
  vi.unstubAllGlobals();
});
