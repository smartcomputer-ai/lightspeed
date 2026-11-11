// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, useLocation } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { VfsTreeEntry } from "@/api";
import { WorkspaceFileTree } from "./workspace-file-tree";

const menuOpened = vi.hoisted(() => vi.fn());
vi.mock("./workspace-transfers", () => ({
  useWorkspaceDropTarget: () => null,
  useWorkspaceEntryDrag: () => ({ draggable: false, onDragStart: () => {} }),
  // Says what its menu is for, and counts the right-clicks that open it.
  WorkspaceContextMenu: ({
    target,
    children,
  }: {
    target: { path: string; kind: string };
    children: ReactNode;
  }) => (
    <div onContextMenu={() => menuOpened()}>
      <span data-menu-target>{`${target.kind}:${target.path}`}</span>
      {children}
    </div>
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
async function render(activePath: string | undefined) {
  await act(async () =>
    root.render(
      <MemoryRouter>
        <WorkspaceFileTree
          entries={entries}
          slug="u"
          workspaceId="ws"
          activePath={activePath}
        />
        <Location />
      </MemoryRouter>,
    ),
  );
}
beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  menuOpened.mockReset();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await render(undefined);
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

async function expandDocs() {
  await act(async () =>
    item("docs").querySelector<HTMLElement>("[data-tree-entry]")!.click(),
  );
}

it("starts with folders collapsed so only the top level renders", () => {
  expect(item("docs").getAttribute("aria-expanded")).toBe("false");
  expect(item("docs/one.txt")).toBeUndefined();
  expect(item("z.txt")).toBeDefined();
});

it("opens the folders holding the file being viewed, also when it changes later", async () => {
  await render("docs/two.txt");
  expect(item("docs").getAttribute("aria-expanded")).toBe("true");
  expect(item("docs/two.txt").getAttribute("aria-selected")).toBe("true");

  // Collapsing stays possible; opening another file inside reopens it.
  await act(async () =>
    item("docs").querySelector<HTMLElement>("[data-tree-entry]")!.click(),
  );
  expect(item("docs").getAttribute("aria-expanded")).toBe("false");
  await render("docs/one.txt");
  expect(item("docs").getAttribute("aria-expanded")).toBe("true");
  expect(item("docs/one.txt")).toBeDefined();
});

it("uses Up and Down to focus visible items instead of scrolling", async () => {
  await expandDocs();
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
  await expandDocs();
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
  await expandDocs();
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

it("opens an item's actions by right-click, Shift+F10 or the context-menu key", async () => {
  const target = () => container.querySelector("[data-menu-target]")!.textContent;
  await expandDocs();
  await act(async () =>
    item("z.txt").dispatchEvent(new MouseEvent("contextmenu", { bubbles: true })),
  );
  expect(menuOpened).toHaveBeenCalledTimes(1);
  expect(target()).toBe("file:z.txt");

  await act(async () => item("docs/one.txt").focus());
  expect((await key("F10", document.activeElement!, true)).defaultPrevented).toBe(true);
  expect(menuOpened).toHaveBeenCalledTimes(2);
  expect(target()).toBe("file:docs/one.txt");
  // F10 alone is not the menu.
  await key("F10");
  expect(menuOpened).toHaveBeenCalledTimes(2);

  await key("ArrowUp");
  await key("ContextMenu");
  expect(menuOpened).toHaveBeenCalledTimes(3);
  expect(target()).toBe("folder:docs");
});
