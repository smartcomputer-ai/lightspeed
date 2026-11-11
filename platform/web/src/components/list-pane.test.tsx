// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it } from "vitest";
import { ListRow, type ListRowTree } from "./list-pane";

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

/// A root with two children, the first with a child of its own, then a
/// second root: the shapes a tree of rows takes.
const ROWS: Array<{ id: string; tree: ListRowTree }> = [
  { id: "root", tree: { depth: 0, children: true } },
  { id: "first", tree: { depth: 1, last: false, lines: [], children: true } },
  { id: "grandchild", tree: { depth: 2, last: true, lines: [true] } },
  { id: "second", tree: { depth: 1, last: true, lines: [] } },
  { id: "other", tree: { depth: 0 } },
];

async function render() {
  await act(async () => root.render(
    <MemoryRouter>
      <ul>{ROWS.map((row) => <ListRow key={row.id} to={`/${row.id}`} title={row.id} tree={row.tree} />)}</ul>
    </MemoryRouter>,
  ));
}
const row = (id: string) => container.querySelector<HTMLElement>(`a[href="/${id}"]`)!;
const lines = (id: string) => row(id).querySelectorAll("[aria-hidden] > span").length;

it("draws no separator inside a tree, only after its last row", async () => {
  await render();
  expect(["root", "first", "grandchild"].map((id) => row(id).className.includes("border-b-0"))).toEqual([true, true, true]);
  expect(row("second").className).not.toContain("border-b-0");
  expect(row("other").className).not.toContain("border-b-0");
});

it("indents each level and hangs each child off its parent's line", async () => {
  await render();
  expect(row("root").style.paddingLeft).toBe("");
  expect(row("first").style.paddingLeft).toBe("2.25rem");
  expect(row("grandchild").style.paddingLeft).toBe("3.125rem");
  // A root's line starts at a dot on its bottom edge (a dotted line and the dot); a
  // middle child has a running line, a branch, the drop from its dot to its
  // children and the dot; the last grandchild an ancestor line, a curve and
  // a dot; the last child a curve and a dot; a lone root nothing.
  expect(lines("root")).toBe(2);
  expect(lines("first")).toBe(4);
  expect(lines("grandchild")).toBe(3);
  expect(lines("second")).toBe(2);
  expect(row("other").querySelector("[aria-hidden]")).toBeNull();
});
