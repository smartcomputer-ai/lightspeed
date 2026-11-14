// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ListRow, ListSelectionButtons, type ListSelectionAction } from "./list-pane";

let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

const actions = (close: () => void): ListSelectionAction[] => [
  { key: "close", label: "Close", count: 2, destructive: true, onSelect: close },
  { key: "delete", label: "Delete", count: 0, destructive: true, onSelect: () => undefined },
];

async function render(rows: Array<{ id: string; checked: boolean }>, rowMenu: () => void, close: () => void, selecting = true) {
  await act(async () => root.render(
    <MemoryRouter>
      <ul>
        {rows.map((row) => (
          <ListRow
            key={row.id}
            to={`/${row.id}`}
            title={row.id}
            entryProps={{ onContextMenu: (event) => { event.preventDefault(); rowMenu(); } }}
            selection={selecting ? { checked: row.checked, onToggle: () => undefined, label: `Select ${row.id}`, actions: actions(close) } : undefined}
          />
        ))}
      </ul>
    </MemoryRouter>,
  ));
}

const rowOf = (title: string) => [...container.querySelectorAll<HTMLElement>("label, a")].find((node) => node.textContent?.includes(title))!;
const rightClick = (element: HTMLElement) => act(async () => {
  element.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 40, clientY: 60 }));
});
const menuItems = () => [...document.querySelectorAll<HTMLElement>('[data-slot="dropdown-menu-item"]')];

it("opens what can be done with the selection on a selected row", async () => {
  const rowMenu = vi.fn();
  const close = vi.fn();
  await render([{ id: "picked", checked: true }, { id: "other", checked: false }], rowMenu, close);
  await rightClick(rowOf("picked"));
  expect(rowMenu).not.toHaveBeenCalled();
  expect(menuItems().map((item) => item.textContent)).toEqual(["Close 2", "Delete 0"]);
  // An action that applies to none of the selection is offered, disabled.
  expect(menuItems()[1]!.hasAttribute("data-disabled")).toBe(true);
  await act(async () => menuItems()[0]!.click());
  expect(close).toHaveBeenCalledOnce();
});

it("opens a row's own menu on a row that is not selected", async () => {
  const rowMenu = vi.fn();
  await render([{ id: "picked", checked: true }, { id: "other", checked: false }], rowMenu, vi.fn());
  await rightClick(rowOf("other"));
  expect(rowMenu).toHaveBeenCalledOnce();
  expect(menuItems()).toHaveLength(0);
});

it("opens a row's own menu outside selection mode", async () => {
  const rowMenu = vi.fn();
  await render([{ id: "plain", checked: false }], rowMenu, vi.fn(), false);
  await rightClick(rowOf("plain"));
  expect(rowMenu).toHaveBeenCalledOnce();
});

it("draws the same actions as the selection bar's buttons", async () => {
  const close = vi.fn();
  await act(async () => root.render(<ListSelectionButtons actions={actions(close)} />));
  const buttons = [...container.querySelectorAll("button")];
  expect(buttons.map((button) => button.textContent)).toEqual(["Close 2", "Delete 0"]);
  expect(buttons[1]!.disabled).toBe(true);
  await act(async () => buttons[0]!.click());
  expect(close).toHaveBeenCalledOnce();
});
