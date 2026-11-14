// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it } from "vitest";
import { Popover, PopoverContent, PopoverTrigger } from "./popover";

let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});

async function render(notch?: boolean) {
  await act(async () => root.render(
    <Popover open>
      <PopoverTrigger>Open</PopoverTrigger>
      <PopoverContent {...(notch === undefined ? {} : { notch })}>Details</PopoverContent>
    </Popover>,
  ));
}

it("points at its trigger with a notch beside the popup, outside what it scrolls", async () => {
  await render();
  const notch = document.querySelector("[data-slot=popover-notch]");
  expect(notch).not.toBeNull();
  expect(document.querySelector("[data-slot=popover-content]")!.contains(notch)).toBe(false);
});

it("leaves the notch off on request", async () => {
  await render(false);
  expect(document.querySelector("[data-slot=popover-content]")).not.toBeNull();
  expect(document.querySelector("[data-slot=popover-notch]")).toBeNull();
});
