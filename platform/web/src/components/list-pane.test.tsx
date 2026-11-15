// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ListLayout, ListRow, type ListExpand, type ListRowTree } from "./list-pane";

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

it("opens and closes a toggle row's children instead of linking", async () => {
  const onToggle = vi.fn();
  await act(async () => root.render(
    <MemoryRouter>
      <ul><ListRow title="src" toggle={{ open: false, onToggle }} /></ul>
    </MemoryRouter>,
  ));
  const button = container.querySelector("button")!;
  expect(container.querySelector("a")).toBeNull();
  expect(button.getAttribute("aria-expanded")).toBe("false");
  await act(async () => button.click());
  expect(onToggle).toHaveBeenCalledOnce();
});

it("puts a row's actions beside its link, not inside it", async () => {
  await act(async () => root.render(
    <MemoryRouter>
      <ul><ListRow to="/file" title="report.pdf" meta="2d" actions={<button type="button">More</button>} /></ul>
    </MemoryRouter>,
  ));
  const link = container.querySelector("a")!;
  const button = [...container.querySelectorAll("button")].find((candidate) => candidate.textContent === "More")!;
  expect(link.contains(button)).toBe(false);
  expect(button.closest("li")).toBe(link.closest("li"));
});

it("draws a muted row's title in its second line's colours", async () => {
  await act(async () => root.render(
    <MemoryRouter>
      <ul>
        <ListRow to="/open" title="Open session" />
        <ListRow to="/closed" title="Closed session" muted />
      </ul>
    </MemoryRouter>,
  ));
  const title = (id: string) => [...row(id).querySelectorAll("span")].reverse().find((span) => span.textContent === `${id[0]!.toUpperCase()}${id.slice(1)} session`)!;
  expect(title("open").className).toContain("text-(--item-foreground-rest)");
  expect(title("open").dataset.muted).toBeUndefined();
  expect(title("closed").className).toContain("text-(--list-second-line-rest)");
  expect(title("closed").className).toContain("group-hover/row:text-muted-foreground");
  expect(title("closed").dataset.muted).toBe("true");
});

describe("dedicated expansion", () => {
  async function renderExpandable(expand: ListExpand, open = false, onToggle = vi.fn()) {
    await act(async () => root.render(
      <MemoryRouter>
        <ListLayout expand={expand}>
          <ul>
            <ListRow to="/parent" title="Parent" expander={{ open, onToggle }} tree={{ depth: 0, children: open }} caretRoom />
            <ListRow to="/leaf" title="Leaf" caretRoom />
            <ListRow to="/alone" title="Alone" />
          </ul>
        </ListLayout>
      </MemoryRouter>,
    ));
    return onToggle;
  }
  const caret = () => container.querySelector<HTMLButtonElement>('[data-slot="list-row-caret"]');
  const rooms = () => container.querySelectorAll('[data-slot="list-row-caret-room"]').length;

  it("keeps the caret's room on a row with a caret and its siblings, and draws the caret beside the link", async () => {
    const onToggle = await renderExpandable("dedicated");
    // Parent and Leaf are siblings with a caret among them; Alone's group has none.
    expect(rooms()).toBe(2);
    expect(row("alone").querySelector('[data-slot="list-row-caret-room"]')).toBeNull();
    expect(caret()!.getAttribute("aria-expanded")).toBe("false");
    expect(caret()!.getAttribute("aria-label")).toBe("Expand");
    // Beside the row's link, never inside it.
    expect(row("parent").contains(caret())).toBe(false);
    await act(async () => caret()!.click());
    expect(onToggle).toHaveBeenCalledOnce();
  });

  it("opens with the right arrow and closes with the left on the focused row", async () => {
    let onToggle = await renderExpandable("dedicated");
    const press = async (key: string) => act(async () => { row("parent").dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true })); });
    await press("ArrowLeft");
    expect(onToggle).not.toHaveBeenCalled();
    await press("ArrowRight");
    expect(onToggle).toHaveBeenCalledOnce();

    onToggle = await renderExpandable("dedicated", true);
    expect(caret()!.getAttribute("aria-label")).toBe("Collapse");
    await press("ArrowRight");
    expect(onToggle).not.toHaveBeenCalled();
    await press("ArrowLeft");
    expect(onToggle).toHaveBeenCalledOnce();
  });

  it("draws no caret or room in a list that does not expand", async () => {
    await renderExpandable("none");
    expect(caret()).toBeNull();
    expect(rooms()).toBe(0);
  });
});

describe("title expansion", () => {
  it("draws the caret after the title's text, outside the link, and keeps no room before the content", async () => {
    const onToggle = vi.fn();
    await act(async () => root.render(
      <MemoryRouter>
        <ListLayout expand="title">
          <ul>
            <ListRow to="/parent" title="Parent" expander={{ open: false, onToggle }} caretRoom />
            <ListRow to="/leaf" title="Leaf" caretRoom />
          </ul>
        </ListLayout>
      </MemoryRouter>,
    ));
    const parent = row("parent");
    const room = parent.querySelector('[data-slot="list-row-caret-room"]')!;
    // The room follows the title's text in the title line.
    expect(room.previousElementSibling!.textContent).toBe("Parent");
    // caretRoom keeps nothing: the caret moves with the text.
    expect(row("leaf").querySelector('[data-slot="list-row-caret-room"]')).toBeNull();
    const caret = container.querySelector<HTMLButtonElement>('[data-slot="list-row-caret"]')!;
    expect(parent.contains(caret)).toBe(false);
    await act(async () => caret.click());
    expect(onToggle).toHaveBeenCalledOnce();
  });
});
