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
  expect(row("first").style.paddingLeft).toBe("2.375rem");
  expect(row("grandchild").style.paddingLeft).toBe("3.25rem");
  // Every parent drops its line from its dot at its centre (a plain dot here,
  // with no status dot): the root has the drop, its dot and the dotted
  // separator on its bottom edge; a middle child a running line, a branch,
  // the drop, the dot and the separator; the last grandchild an ancestor
  // line, a curve, a dot and a separator (its grandparent's line runs on);
  // the last child a curve and a dot, ending the tree on a solid separator;
  // a lone root nothing.
  expect(lines("root")).toBe(3);
  expect(lines("first")).toBe(5);
  expect(lines("grandchild")).toBe(4);
  expect(lines("second")).toBe(2);
  expect(row("other").querySelector("[aria-hidden]")).toBeNull();
});
