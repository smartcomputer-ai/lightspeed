// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, useLocation } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { VfsTreeEntry } from "@/api";
import { WorkspaceFileTree } from "./workspace-file-tree";

const menuClick = vi.hoisted(() => vi.fn());
vi.mock("./workspace-transfers", () => ({
  useWorkspaceDropTarget: () => null,
  WorkspaceActionsMenu: ({
    path,
    tabIndex,
  }: {
    path: string;
    tabIndex?: number;
  }) => (
    <button
      data-workspace-actions
      tabIndex={tabIndex}
      onClick={() => menuClick(path)}
    >
      More
    </button>
  ),
}));
let root: Root;
let container: HTMLDivElement;
const file: VfsTreeEntry = {
  kind: "file",
  blob_ref: "blob",
  executable: false,
  size_bytes: 1,
};
const entries: Record<string, VfsTreeEntry> = {
  docs: { kind: "directory", entries: { "one.txt": file, "two.txt": file } },
  "z.txt": file,
};
function Location() {
  return <output>{useLocation().pathname}</output>;
}
beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  menuClick.mockReset();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () =>
    root.render(
      <MemoryRouter>
        <WorkspaceFileTree
          entries={entries}
          slug="u"
          workspaceId="ws"
          activePath={undefined}
        />
        <Location />
      </MemoryRouter>,
    ),
  );
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});
const item = (path: string) =>
  [...container.querySelectorAll<HTMLElement>('[role="treeitem"]')].find(
    (item) => item.dataset.treePath === path,
  )!;
async function key(
  key: string,
  target: Element = document.activeElement!,
  shiftKey = false,
) {
  const event = new KeyboardEvent("keydown", {
    key,
    shiftKey,
    bubbles: true,
    cancelable: true,
  });
  await act(async () => target.dispatchEvent(event));
  return event;
}

it("uses Up and Down to focus visible items instead of scrolling", async () => {
  expect(item("docs").tabIndex).toBe(0);
  await act(async () => item("docs").focus());
  expect((await key("ArrowDown")).defaultPrevented).toBe(true);
  expect(document.activeElement).toBe(item("docs/one.txt"));
  await key("ArrowDown");
  expect(document.activeElement).toBe(item("docs/two.txt"));
  await key("ArrowUp");
  expect(document.activeElement).toBe(item("docs/one.txt"));
  expect(item("docs/one.txt").tabIndex).toBe(0);
  expect(item("docs").tabIndex).toBe(-1);
  await key("End");
  expect(document.activeElement).toBe(item("z.txt"));
  await key("Home");
  expect(document.activeElement).toBe(item("docs"));
});

it("collapses and expands folders and skips their hidden children", async () => {
  await act(async () => item("docs/one.txt").focus());
  await key("ArrowLeft");
  expect(document.activeElement).toBe(item("docs"));
  await key("ArrowLeft");
  expect(item("docs").getAttribute("aria-expanded")).toBe("false");
  await key("ArrowDown");
  expect(document.activeElement).toBe(item("z.txt"));
  await key("ArrowUp");
  await key("ArrowRight");
  expect(item("docs").getAttribute("aria-expanded")).toBe("true");
  await key("ArrowRight");
  expect(document.activeElement).toBe(item("docs/one.txt"));
});

it("handles arrows after focusing a file link and opens the focused file with Enter", async () => {
  await act(async () =>
    item("docs/one.txt").querySelector<HTMLElement>("a")!.focus(),
  );
  await key("ArrowDown");
  expect(document.activeElement).toBe(item("docs/two.txt"));
  await key("Enter");
  expect(container.querySelector("output")!.textContent).toBe(
    "/u/u/workspaces/ws/files/docs/two.txt",
  );
});

it("offers the focused item's menu via Tab or Shift+F10 without stealing its arrow keys", async () => {
  await act(async () => item("docs/one.txt").focus());
  const menu = item("docs/one.txt").querySelector<HTMLElement>(
    "[data-workspace-actions]",
  )!;
  expect(menu.tabIndex).toBe(0);
  expect(
    item("z.txt").querySelector<HTMLElement>("[data-workspace-actions]")!
      .tabIndex,
  ).toBe(-1);
  await key("F10", document.activeElement!, true);
  expect(menuClick).toHaveBeenCalledWith("docs/one.txt");
  await act(async () => menu.focus());
  expect((await key("ArrowDown")).defaultPrevented).toBe(false);
  expect(document.activeElement).toBe(menu);
});
