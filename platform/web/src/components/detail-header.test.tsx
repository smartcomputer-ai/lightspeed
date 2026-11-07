// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { DetailHeader, type DetailHeaderAction } from "./detail-header";

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

async function render(actions: DetailHeaderAction[]) {
  await act(async () =>
    root.render(
      <MemoryRouter>
        <DetailHeader back={{ to: "/list", label: "Back to list" }} title="Item" details="r3" actions={actions} />
      </MemoryRouter>,
    ),
  );
}

const button = (name: string) =>
  [...container.querySelectorAll("button")].find(
    (element) => element.getAttribute("aria-label") === name || element.textContent === name,
  );

it("shows each action as a button, with a ⋯ menu that takes them over where narrow", async () => {
  const save = vi.fn();
  await render([
    { key: "save", label: "Save", text: "Save", primary: true, onSelect: save },
    { key: "delete", label: "Delete item", icon: <svg />, destructive: true },
  ]);
  expect(container.querySelector("h1")?.textContent).toBe("Item");
  expect(container.querySelector('a[aria-label="Back to list"]')).not.toBeNull();
  expect(button("Delete item")).toBeDefined();
  expect(button("More actions")).toBeDefined();
  // The inline buttons hide below the container breakpoint, the menu above it.
  expect(button("Save")?.closest("span")?.className).toContain("hidden");
  expect(button("More actions")?.closest("span")?.className).toContain("@xl/detail-header:hidden");
  await act(async () => button("Save")!.click());
  expect(save).toHaveBeenCalledOnce();
});

it("always shows custom actions and adds no ⋯ menu when nothing collapses", async () => {
  await render([{ kind: "custom", key: "own", node: <button aria-label="Own menu" /> }]);
  expect(button("Own menu")).toBeDefined();
  expect(button("More actions")).toBeUndefined();
});
