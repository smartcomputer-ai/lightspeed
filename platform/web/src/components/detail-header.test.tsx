// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { DetailHeader, type HeaderTool, type MainHeaderTool } from "./detail-header";

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

async function render(tools: MainHeaderTool[], subtools: HeaderTool[] = []) {
  await act(async () =>
    root.render(
      <MemoryRouter>
        <DetailHeader back={{ to: "/list", label: "Back to list" }} title="Item" details="r3" tools={tools} subtools={subtools} />
      </MemoryRouter>,
    ),
  );
}

const button = (name: string) =>
  [...container.querySelectorAll("button")].find(
    (element) =>
      element.getAttribute("aria-label") === name ||
      (element.querySelector("[data-save-label]:not([aria-hidden])") ?? element).textContent === name,
  );
const mainTools = () => container.querySelector("[data-header-tools]");

it("joins the main tools in one group at the far right, shown at every width", async () => {
  const save = vi.fn();
  await render([
    { kind: "save", key: "save", label: "Save", dirty: true, saving: false, onSave: save },
    { key: "delete", label: "Delete item", icon: <svg />, destructive: true },
    { kind: "custom", key: "own", node: <button data-slot="button" aria-label="Own menu" /> },
  ]);
  expect(container.querySelector("h1")?.textContent).toBe("Item");
  expect(container.querySelector('a[aria-label="Back to list"]')).not.toBeNull();
  // One group, in order, last in the header; nothing hides where narrow.
  expect(mainTools()?.getAttribute("role")).toBe("group");
  expect([...mainTools()!.children].map((child) => child.getAttribute("aria-label") ?? child.getAttribute("data-save-state")))
    .toEqual(["dirty", "Delete item", "Own menu"]);
  expect(mainTools()?.parentElement?.lastElementChild).toBe(mainTools());
  expect(mainTools()?.className).not.toContain("hidden");
  // In the group, an icon tool takes its look: destructive here, not ghost.
  expect(button("Delete item")?.className).toContain("bg-destructive/10");
  expect(button("More tools")).toBeUndefined();
  await act(async () => button("Save")!.click());
  expect(save).toHaveBeenCalledOnce();
});

it("puts subtools left of the main tools, folding them into a ⋯ menu where narrow", async () => {
  const choose = vi.fn();
  await render(
    [{ kind: "custom", key: "own", node: <button data-slot="button" aria-label="Own menu" /> }],
    [{ kind: "choice", key: "view", label: "View", value: "a", options: [{ value: "a", label: "A" }, { value: "b", label: "B" }], onChange: choose, appearance: "toggle" }],
  );
  const subtools = container.querySelector("[data-header-subtools]")!;
  expect(subtools.nextElementSibling).toBe(mainTools());
  // The inline toggle hides below the container breakpoint, the menu above it.
  expect(button("B")?.closest("span")?.className).toContain("hidden");
  expect(button("More tools")?.closest("span")?.className).toContain("@xl/detail-header:hidden");
  await act(async () => button("B")!.click());
  expect(choose).toHaveBeenCalledWith("b");
});
