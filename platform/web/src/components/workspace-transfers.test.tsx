// @vitest-environment jsdom
import { act, type ReactNode, type ReactElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { WorkspacesPage } from "@/pages/WorkspacesPage";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ApiError, type VfsTreeEntry } from "@/api";
import {
  WorkspaceTransfers,
  WorkspaceActionsMenu,
  WorkspaceDropArea,
} from "./workspace-transfers";
import { WorkspaceFileTree } from "./workspace-file-tree";

const mocks = vi.hoisted(() => ({
  api: vi.fn(),
  editable: true,
  removed: vi.fn(),
  renamed: vi.fn(),
  newFile: vi.fn(),
}));
vi.mock("@/api", async (original) => ({
  ...(await original<typeof import("@/api")>()),
  api: mocks.api,
}));
vi.mock("@/lib/permissions", () => ({
  useActionPermissions: () => ({ can: () => mocks.editable }),
}));
vi.mock("@/lib/universes", () => ({
  useActiveUniverse: () => ({
    universe: { id: "u" },
    slug: "test",
    isLoading: false,
  }),
}));
// Exercise our menu actions without Floating UI's popup geometry in jsdom.
vi.mock("@/components/ui/dropdown-menu", async () => {
  const { createContext, useContext, useState, cloneElement } =
    await import("react");
  const Menu = createContext({ open: false, setOpen: (_open: boolean) => {} });
  return {
    DropdownMenu: ({ children }: { children: ReactNode }) => {
      const [open, setOpen] = useState(false);
      return (
        <Menu.Provider value={{ open, setOpen }}>{children}</Menu.Provider>
      );
    },
    DropdownMenuTrigger: ({
      render,
      children,
    }: {
      render: ReactElement;
      children: ReactNode;
    }) => {
      const { open, setOpen } = useContext(Menu);
      return cloneElement(
        render as ReactElement<{ onClick: () => void }>,
        { onClick: () => setOpen(!open) },
        children,
      );
    },
    DropdownMenuContent: ({ children }: { children: ReactNode }) =>
      useContext(Menu).open ? <div role="menu">{children}</div> : null,
    DropdownMenuItem: ({
      children,
      onClick,
      disabled,
    }: {
      children: ReactNode;
      onClick: () => void;
      disabled?: boolean;
    }) => {
      const { setOpen } = useContext(Menu);
      return (
        <button
          role="menuitem"
          disabled={disabled}
          onClick={() => {
            setOpen(false);
            onClick();
          }}
        >
          {children}
        </button>
      );
    },
    DropdownMenuSeparator: () => <hr />,
  };
});
let root: Root;
let container: HTMLDivElement;
let entries: Record<string, VfsTreeEntry>;
const originalPdfSupport = Object.getOwnPropertyDescriptor(
  navigator,
  "pdfViewerEnabled",
);
const file: VfsTreeEntry = {
  kind: "file",
  blob_ref: "old",
  size_bytes: 1,
  executable: false,
};
const tree = (revision = 3) => ({
  workspace: { revision },
  manifest: { root: { entries }, totals: { files: 1, bytes: 1 } },
});
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mocks.editable = true;
  mocks.removed.mockReset();
  mocks.renamed.mockReset();
  mocks.newFile.mockReset();
  entries = { docs: { kind: "directory", entries: { "a #?.txt": file } } };
  mocks.api.mockReset().mockImplementation(async () => tree());
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  if (originalPdfSupport)
    Object.defineProperty(navigator, "pdfViewerEnabled", originalPdfSupport);
  else Reflect.deleteProperty(navigator, "pdfViewerEnabled");
});
const settle = () => new Promise((resolve) => setTimeout(resolve, 40));
async function render(withTree = false) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  await act(async () =>
    root.render(
      <QueryClientProvider client={client}>
        <WorkspaceTransfers
          universeId="u"
          workspaceId="ws"
          onRemoved={mocks.removed}
          onRenamed={mocks.renamed}
          onNewFile={mocks.newFile}
        >
          <WorkspaceActionsMenu kind="workspace" />
          {withTree ? (
            <MemoryRouter>
              <WorkspaceDropArea>
                <WorkspaceFileTree
                  entries={entries}
                  slug="test"
                  workspaceId="ws"
                  activePath={undefined}
                />
              </WorkspaceDropArea>
            </MemoryRouter>
          ) : (
            <>
              <div className="group/tree-row" data-workspace-folder="docs">
                <WorkspaceActionsMenu path="docs" kind="folder" />
              </div>
              <WorkspaceActionsMenu path="docs/a #?.txt" kind="file" />
            </>
          )}
        </WorkspaceTransfers>
      </QueryClientProvider>,
    ),
  );
  await act(settle);
}
async function click(element: Element | null | undefined) {
  if (!element) throw new Error("Expected a clickable element");
  await act(async () => {
    (element as HTMLElement).click();
    await settle();
  });
}
const button = (text: string) =>
  Array.from(document.querySelectorAll("button")).find(
    (button) => button.textContent === text,
  );
const item = (text: string) =>
  Array.from(document.querySelectorAll('[role="menuitem"]')).find(
    (item) => item.textContent === text,
  );
async function menu(label: string) {
  await click(container.querySelector(`[aria-label="${label}"]`));
}
async function select(files: File[], replacement = false) {
  const input = container.querySelector(
    `[aria-label="${replacement ? "Select replacement file" : "Select files to upload"}"]`,
  )!;
  Object.defineProperty(input, "files", { configurable: true, value: files });
  await act(async () => {
    input.dispatchEvent(new Event("change", { bubbles: true }));
    await settle();
  });
}
const writes = () =>
  mocks.api.mock.calls.filter(([method]) => method === "POST");

async function drag(type: string, target: Element, types = ["Files"]) {
  const event = new Event(type, { bubbles: true, cancelable: true });
  const transfer = {
    types,
    dropEffect: "none",
    items: [{ kind: "file", getAsFile: () => new File(["hello"], "new.txt") }],
  };
  Object.defineProperty(event, "dataTransfer", { value: transfer });
  await act(async () => {
    target.dispatchEvent(event);
    await settle();
  });
  return transfer;
}

it("highlights the exact drop destination and uploads to the folder shown", async () => {
  entries = {
    docs: {
      kind: "directory",
      entries: {
        nested: { kind: "directory", entries: { "existing.txt": file } },
      },
    },
  };
  await render(true);
  const rootArea = container.querySelector("[data-workspace-root]")!;
  const folderRow = (path: string) =>
    container.querySelector(`[data-tree-path="${path}"] > div`)!;
  const highlighted = () =>
    container.querySelectorAll('[data-workspace-drop-target="true"]');
  await drag("dragenter", rootArea);
  expect([...highlighted()]).toEqual([rootArea]);
  expect(container.querySelector('[role="status"]')?.textContent).toContain(
    "workspace root /",
  );
  await drag("dragover", folderRow("docs").querySelector("span")!);
  expect([...highlighted()]).toEqual([folderRow("docs")]);
  expect(container.querySelector('[role="status"]')?.textContent).toContain(
    "/docs",
  );
  const nested = folderRow("docs/nested");
  await drag("dragover", nested.querySelector("button")!);
  expect([...highlighted()]).toEqual([nested]);
  expect(container.querySelector('[role="status"]')?.textContent).toContain(
    "/docs/nested",
  );
  // Files inside a folder target that folder, not the file itself.
  const fileLink = container.querySelector(
    '[data-tree-path="docs/nested/existing.txt"] a',
  )!;
  expect((await drag("dragover", fileLink)).dropEffect).toBe("copy");
  expect([...highlighted()]).toEqual([nested]);
  await drag("drop", fileLink);
  await waitForWrites();
  expect(writes()[0]?.[2].entries[0].path).toBe("docs/nested/new.txt");
  expect(highlighted()).toHaveLength(0);
  expect(container.querySelector('[role="status"]')).toBeNull();
});

it("switches back to the root destination and clears the hint when dragging leaves or ends", async () => {
  await render(true);
  const rootArea = container.querySelector("[data-workspace-root]")!;
  const row = container.querySelector('[data-tree-path="docs"] > div')!;
  await drag("dragenter", row);
  await drag("dragover", rootArea);
  expect(rootArea.getAttribute("data-workspace-drop-target")).toBe("true");
  expect(row.hasAttribute("data-workspace-drop-target")).toBe(false);
  expect(container.querySelector('[role="status"]')?.textContent).toContain(
    "workspace root /",
  );
  await drag("dragleave", rootArea);
  expect(container.querySelector('[role="status"]')).toBeNull();
  expect(
    container.querySelector('[data-workspace-drop-target="true"]'),
  ).toBeNull();
  await drag("dragenter", row);
  await drag("dragend", row);
  expect(container.querySelector('[role="status"]')).toBeNull();
});

it("does not advertise a drop destination for viewers or text drags", async () => {
  await render(true);
  const rootArea = container.querySelector("[data-workspace-root]")!;
  await drag("dragenter", rootArea, ["text/plain"]);
  expect(container.querySelector('[role="status"]')).toBeNull();
  mocks.editable = false;
  await render(true);
  await drag("dragenter", rootArea);
  expect((await drag("dragover", rootArea)).dropEffect).toBe("none");
  expect(container.querySelector('[role="status"]')).toBeNull();
  expect(
    container.querySelector('[data-workspace-drop-target="true"]'),
  ).toBeNull();
});
async function waitForWrites(count = 1) {
  await act(async () => {
    await vi.waitFor(() => expect(writes()).toHaveLength(count));
  });
}

async function folderName(name: string) {
  const input = document.querySelector<HTMLInputElement>(
    '[role="dialog"] input',
  )!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!.call(input, name);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

it.each([
  ["Folder actions: docs", "docs", "renamed"],
  ["File actions: docs/a #?.txt", "docs/a #?.txt", "docs/renamed"],
])(
  "renames from %s using the revision shown when the dialog opened",
  async (label, path, destination) => {
    await render();
    await menu(label);
    await click(item("Rename…"));
    const input = document.querySelector<HTMLInputElement>(
      '[role="dialog"] input',
    )!;
    expect(input.value).toBe(path.split("/").at(-1));
    expect((button("Rename") as HTMLButtonElement).disabled).toBe(true);
    expect(input.selectionStart).toBe(0);
    expect(input.selectionEnd).toBe(
      label.startsWith("File")
        ? input.value.lastIndexOf(".")
        : input.value.length,
    );
    await folderName("renamed");
    await click(button("Rename"));
    expect(writes()).toEqual([
      [
        "POST",
        "/api/v1/universes/u/workspaces/ws/rename",
        { path, name: "renamed", expectedRevision: 3 },
      ],
    ]);
    expect(mocks.renamed).toHaveBeenCalledWith(path, destination);
    expect(document.querySelector('[role="dialog"]')).toBeNull();
  },
);

it("keeps the rename dialog open for invalid names, conflicts and server failures", async () => {
  await render();
  await menu("File actions: docs/a #?.txt");
  await click(item("Rename…"));
  await folderName("../escape");
  await click(button("Rename"));
  expect(document.querySelector('[role="alert"]')).not.toBeNull();
  expect(writes()).toHaveLength(0);
  await folderName("new.txt");
  mocks.api.mockImplementation(async (method) => {
    if (method === "POST")
      throw new ApiError(409, {
        error: "Workspace changed. Close this dialog and try again.",
      });
    return tree(4);
  });
  await click(button("Rename"));
  expect(document.querySelector('[role="alert"]')?.textContent).toContain(
    "Workspace changed",
  );
  expect(
    document.querySelector<HTMLInputElement>('[role="dialog"] input')?.value,
  ).toBe("new.txt");
  expect(mocks.renamed).not.toHaveBeenCalled();
  await click(button("Cancel"));
  await menu("Folder actions: docs");
  await click(item("Rename…"));
  await folderName("new");
  mocks.api.mockImplementation(async () => tree(4));
  await click(button("Rename"));
  expect(writes().at(-1)?.[2].expectedRevision).toBe(4);
});

it.each([
  ["Workspace actions", ""],
  ["Folder actions: docs", "docs"],
])(
  "opens New file in the destination chosen from %s",
  async (label, parent) => {
    await render();
    await menu(label);
    await click(item("New file"));
    expect(mocks.newFile).toHaveBeenCalledWith(parent, 3);
  },
);

it.each([
  ["Workspace actions", "empty"],
  ["Folder actions: docs", "docs/empty"],
])("creates an empty folder from %s", async (label, path) => {
  await render();
  await menu(label);
  await click(item("New folder"));
  await folderName("empty");
  // Read a fresh revision when submitting, not the menu's original snapshot.
  mocks.api.mockImplementation(async () => tree(7));
  await click(button("Create folder"));
  expect(writes()).toEqual([
    [
      "POST",
      "/api/v1/universes/u/workspaces/ws/upload",
      {
        expectedRevision: 7,
        replace: false,
        entries: [{ kind: "directory", path }],
      },
    ],
  ]);
  expect(document.querySelector('[role="dialog"]')).toBeNull();
});

it("rejects invalid and existing folder names without writing and lets the user correct them", async () => {
  await render();
  await menu("Workspace actions");
  await click(item("New folder"));
  await folderName("../escape");
  await click(button("Create folder"));
  expect(document.querySelector('[role="alert"]')?.textContent).toContain(
    "without slashes",
  );
  await folderName("docs");
  await click(button("Create folder"));
  expect(document.querySelector('[role="alert"]')?.textContent).toContain(
    "already exists",
  );
  expect(writes()).toHaveLength(0);
  await folderName("new name");
  await click(button("Create folder"));
  expect(writes()).toHaveLength(1);
  expect(document.querySelector('[role="dialog"]')).toBeNull();
});

it("keeps the name after a create conflict so retry can use the latest revision", async () => {
  await render();
  await menu("Folder actions: docs");
  await click(item("New folder"));
  await folderName("empty");
  mocks.api.mockImplementation(async (method) => {
    if (method === "POST")
      throw new ApiError(409, { error: "Workspace changed" });
    return tree(4);
  });
  await click(button("Create folder"));
  expect(document.querySelector('[role="alert"]')).not.toBeNull();
  expect(
    document.querySelector<HTMLInputElement>('[role="dialog"] input')?.value,
  ).toBe("empty");
  mocks.api.mockImplementation(async () => tree(5));
  await click(button("Create folder"));
  expect(writes().at(-1)?.[2].expectedRevision).toBe(5);
  expect(document.querySelector('[role="dialog"]')).toBeNull();
});

it("does not recreate a parent folder deleted while the creation dialog was open", async () => {
  await render();
  await menu("Folder actions: docs");
  await click(item("New folder"));
  await folderName("empty");
  entries = {};
  await click(button("Create folder"));
  expect(document.querySelector('[role="alert"]')?.textContent).toContain(
    "parent folder no longer exists",
  );
  expect(writes()).toHaveLength(0);
});

it("puts workspace actions in one menu, and uploads new files without a modal", async () => {
  await render();
  expect(document.querySelector('[role="menuitem"]')).toBeNull();
  expect(button("Upload files")).toBeUndefined();
  await menu("Workspace actions");
  expect(item("New file")).toBeDefined();
  expect(item("Upload folder")).toBeDefined();
  expect(item("Download as ZIP")).toBeDefined();
  await click(item("Upload files"));
  await select([
    new File([new Uint8Array([0, 255])], "new.pdf", {
      type: "application/pdf",
    }),
  ]);
  await waitForWrites();
  expect(writes()[0]).toEqual([
    "POST",
    "/api/v1/universes/u/workspaces/ws/upload",
    {
      expectedRevision: 3,
      replace: false,
      entries: [
        {
          kind: "file",
          path: "new.pdf",
          contentBase64: "AP8=",
          mediaType: "application/pdf",
        },
      ],
    },
  ]);
  expect(document.querySelector('[role="dialog"]')).toBeNull();
  expect(container.textContent).not.toContain("Upload complete");
});

it("uploads dropped files into the folder under the pointer without asking", async () => {
  await render();
  const event = new Event("drop", { bubbles: true, cancelable: true });
  Object.defineProperty(event, "dataTransfer", {
    value: {
      types: ["Files"],
      items: [
        { kind: "file", getAsFile: () => new File(["hello"], "note.txt") },
      ],
    },
  });
  await act(async () => {
    container.querySelector("[data-workspace-folder]")!.dispatchEvent(event);
    await settle();
  });
  expect(event.defaultPrevented).toBe(true);
  await waitForWrites();
  expect(writes()[0]?.[2].entries[0].path).toBe("docs/note.txt");
  expect(document.querySelector('[role="dialog"]')).toBeNull();
});

it("keeps progress inside the action menu and adds no status strip", async () => {
  await render();
  let finish!: () => void;
  const pending = new Promise<void>((resolve) => {
    finish = resolve;
  });
  mocks.api.mockImplementation(async (method: string) => {
    if (method === "POST") await pending;
    return tree();
  });
  await select([new File(["new"], "new.txt")]);
  const statuses = [...container.querySelectorAll('[role="status"]')];
  expect(statuses.length).toBeGreaterThan(0);
  for (const status of statuses) {
    expect(status.classList.contains("sr-only")).toBe(true);
    expect(status.closest("button")).not.toBeNull();
  }
  expect(document.querySelector('[role="dialog"]')).toBeNull();
  await act(async () => {
    finish();
    await settle();
  });
  expect(container.querySelector('[role="status"]')).toBeNull();
  expect(container.textContent).not.toContain("Upload complete");
});

it("reports download failures in a dialog", async () => {
  await render();
  vi.stubGlobal(
    "fetch",
    vi.fn(async () =>
      Response.json({ error: "File no longer exists" }, { status: 404 }),
    ),
  );
  await menu("File actions: docs/a #?.txt");
  await click(item("Download"));
  expect(document.querySelector('[role="dialog"]')?.textContent).toContain(
    "File no longer exists",
  );
  expect(container.querySelector('[role="status"]')).toBeNull();
});

it("replaces the selected file at its original path after confirmation", async () => {
  await render();
  await menu("File actions: docs/a #?.txt");
  expect(item("Download")).toBeDefined();
  expect(item("Delete file…")).toBeDefined();
  await click(item("Upload replacement…"));
  await select([new File(["new"], "different-name.txt")], true);
  expect(writes()).toHaveLength(0);
  expect(document.body.textContent).toContain("Replace existing files?");
  expect(document.body.textContent).toContain("docs/a #?.txt");
  await click(button("Replace and upload"));
  await waitForWrites();
  expect(writes()[0]?.[2]).toMatchObject({
    replace: true,
    expectedRevision: 3,
    entries: [{ path: "docs/a #?.txt", contentBase64: "bmV3" }],
  });
});

it("can skip existing files and upload the rest", async () => {
  await render();
  await menu("Folder actions: docs");
  await click(item("Upload files"));
  await select([
    new File(["replacement"], "a #?.txt"),
    new File(["new"], "new.txt"),
  ]);
  expect(writes()).toHaveLength(0);
  await click(button("Skip existing"));
  await waitForWrites();
  expect(writes()[0]?.[2]).toMatchObject({
    replace: false,
    entries: [{ path: "docs/new.txt" }],
  });
  expect(writes()[0]?.[2].entries).toHaveLength(1);
});

it("rechecks collisions after a concurrent edit instead of silently overwriting", async () => {
  await render();
  let failed = false;
  mocks.api.mockImplementation(async (method: string) => {
    if (method === "POST" && !failed) {
      failed = true;
      entries = { "new.txt": file };
      throw new ApiError(409, { error: "Workspace changed" });
    }
    return tree(failed ? 4 : 3);
  });
  await select([new File(["new"], "new.txt")]);
  expect(document.querySelector('[role="alert"]')?.textContent).toContain(
    "Workspace changed",
  );
  await click(button("Retry"));
  expect(writes()).toHaveLength(1);
  expect(document.body.textContent).toContain("Replace existing files?");
  await click(button("Replace and upload"));
  expect(writes()[1]?.[2]).toMatchObject({
    replace: true,
    expectedRevision: 4,
  });
});

it("confirms recursive folder deletion and reports the removed path", async () => {
  await render();
  await menu("Folder actions: docs");
  expect(item("Upload folder")).toBeDefined();
  expect(item("Download as ZIP")).toBeDefined();
  await click(item("Delete folder…"));
  expect(document.body.textContent).toContain(
    "all files and folders inside it",
  );
  expect(mocks.api.mock.calls.some(([method]) => method === "DELETE")).toBe(
    false,
  );
  await click(button("Delete folder"));
  expect(mocks.api).toHaveBeenCalledWith(
    "DELETE",
    "/api/v1/universes/u/workspaces/ws/entries?path=docs&expectedRevision=3",
  );
  expect(mocks.removed).toHaveBeenCalledWith("docs");
});

it("offers viewers only downloads", async () => {
  mocks.editable = false;
  await render();
  await menu("Folder actions: docs");
  expect(item("Download as ZIP")).toBeDefined();
  expect(item("Upload files")).toBeUndefined();
  expect(item("New folder")).toBeUndefined();
  expect(item("New file")).toBeUndefined();
  expect(item("Rename…")).toBeUndefined();
  expect(item("Delete folder…")).toBeUndefined();
});

it("downloads from the file menu with the original filename", async () => {
  await render();
  const fetchMock = vi.fn(async () => new Response(new Blob(["content"])));
  vi.stubGlobal("fetch", fetchMock);
  URL.createObjectURL = vi.fn(() => "blob:download");
  URL.revokeObjectURL = vi.fn();
  const names: string[] = [];
  vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (
    this: HTMLAnchorElement,
  ) {
    names.push(this.download);
  });
  await menu("File actions: docs/a #?.txt");
  await click(item("Download"));
  expect(fetchMock).toHaveBeenCalledWith(
    "/api/v1/universes/u/workspaces/ws/download?path=docs%2Fa+%23%3F.txt",
    { credentials: "same-origin" },
  );
  expect(names).toEqual(["a #?.txt"]);
});

it("places the open file’s actions in the tree, not its detail header", async () => {
  const workspace = {
    workspaceId: "ws",
    displayName: "Documents",
    revision: 3,
    files: 1,
  };
  mocks.api.mockImplementation(async (_method: string, path: string) => {
    if (path.endsWith("/workspaces")) return [workspace];
    if (path.endsWith("/tree")) return { ...tree(), workspace };
    if (path.includes("/files/")) return { bytesBase64: "eA==", bytes: 1 };
    throw new Error(`Unexpected request: ${path}`);
  });
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  await act(async () =>
    root.render(
      <QueryClientProvider client={client}>
        <MemoryRouter
          initialEntries={["/u/test/workspaces/ws/files/docs/a%20%23%3F.txt"]}
        >
          <Routes>
            <Route
              path="/u/:slug/workspaces/:workspaceId/files/*"
              element={<WorkspacesPage admin={false} />}
            />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    ),
  );
  await act(settle);
  const menus = container.querySelectorAll(
    '[aria-label="File actions: docs/a #?.txt"]',
  );
  expect(menus).toHaveLength(1);
  expect(menus[0]!.closest("li")).not.toBeNull();
  expect(
    container.querySelector('header [aria-label^="File actions:"]'),
  ).toBeNull();
  expect(
    container.querySelector('textarea[aria-label="File contents"]'),
  ).not.toBeNull();
  const workspacePicker = container.querySelector('[aria-label="Workspace"]')!;
  const actions = container.querySelector('[aria-label="Workspace actions"]')!;
  expect(actions.textContent).toBe("");
  expect(workspacePicker.parentElement!.parentElement).toBe(
    actions.parentElement,
  );
  expect(
    container.querySelector('[aria-label="Workspace information"]'),
  ).toBeNull();
  await menu("Workspace actions");
  expect(
    document.querySelector('[aria-label="Workspace information"] dd')
      ?.textContent,
  ).toBe("1");
});

it.each(["", "docs"])(
  "creates and opens a new file relative to folder '%s'",
  async (parent) => {
    let revision = 3;
    const workspace = () => ({
      workspaceId: "ws",
      displayName: "Documents",
      revision,
      files: revision - 2,
    });
    const destination = `${parent ? `${parent}/` : ""}nested/note #?.txt`;
    mocks.api.mockImplementation(
      async (
        method: string,
        path: string,
        body?: { entries: { path: string }[] },
      ) => {
        if (path.endsWith("/workspaces")) return [workspace()];
        if (path.endsWith("/tree"))
          return { ...tree(), workspace: workspace() };
        if (path.endsWith("/upload") && method === "POST") {
          let directory = entries;
          const segments = body!.entries[0]!.path.split("/");
          const name = segments.pop()!;
          for (const segment of segments) {
            const entry = (directory[segment] ??= {
              kind: "directory",
              entries: {},
            });
            if (entry.kind !== "directory") throw new Error("Expected folder");
            directory = entry.entries;
          }
          directory[name] = { ...file, size_bytes: 0, blob_ref: "empty" };
          revision++;
          return { workspace: workspace() };
        }
        if (path.includes("/files/"))
          return { bytesBase64: path.includes("note") ? "" : "eA==", bytes: 0 };
        throw new Error(`Unexpected request: ${path}`);
      },
    );
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    });
    await act(async () =>
      root.render(
        <QueryClientProvider client={client}>
          <MemoryRouter
            initialEntries={["/u/test/workspaces/ws/files/docs/a%20%23%3F.txt"]}
          >
            <Routes>
              <Route
                path="/u/:slug/workspaces/:workspaceId/files/*"
                element={<WorkspacesPage admin={false} />}
              />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      ),
    );
    await act(settle);
    await menu(parent ? `Folder actions: ${parent}` : "Workspace actions");
    await click(item("New file"));
    expect(document.querySelector('[role="dialog"]')?.textContent).toContain(
      `Create a file in ${parent || "the workspace root"}`,
    );
    await folderName("../escape.txt");
    await click(button("Create"));
    expect(writes()).toHaveLength(0);
    await folderName("nested/note #?.txt");
    await click(button("Create"));
    expect(writes()).toEqual([
      [
        "POST",
        "/api/v1/universes/u/workspaces/ws/upload",
        {
          expectedRevision: 3,
          replace: false,
          entries: [
            {
              kind: "file",
              path: destination,
              contentBase64: "",
              mediaType: "text/plain",
            },
          ],
        },
      ],
    ]);
    await vi.waitFor(async () => {
      await act(settle);
      expect(
        container
          .querySelector('[role="treeitem"][aria-selected="true"]')
          ?.getAttribute("data-tree-path"),
      ).toBe(destination);
      expect(container.querySelector("textarea")?.value).toBe("");
    });
    expect(document.querySelector('[role="dialog"]')).toBeNull();
  },
);

it.each(["file", "folder"])(
  "keeps an open file and unsaved edits when renaming its %s",
  async (kind) => {
    entries["other.txt"] = file;
    const workspace = {
      workspaceId: "ws",
      displayName: "Documents",
      revision: 3,
      files: 1,
    };
    let currentWorkspace = workspace;
    const destination =
      kind === "file" ? "docs/renamed #?.txt" : "renamed #?/a #?.txt";
    mocks.api.mockImplementation(
      async (method: string, path: string, body?: { name: string }) => {
        if (path.endsWith("/workspaces")) return [currentWorkspace];
        if (path.endsWith("/tree"))
          return { ...tree(), workspace: currentWorkspace };
        if (path.endsWith("/rename") && method === "POST") {
          if (kind === "folder") {
            entries = { [body!.name]: entries.docs!, "other.txt": file };
          } else {
            entries = {
              docs: { kind: "directory", entries: { [body!.name]: file } },
              "other.txt": file,
            };
          }
          currentWorkspace = { ...workspace, revision: 4 };
          return { workspace: currentWorkspace };
        }
        if (path.includes("/files/")) return { bytesBase64: "eA==", bytes: 1 };
        throw new Error(`Unexpected request: ${path}`);
      },
    );
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    });
    await act(async () =>
      root.render(
        <QueryClientProvider client={client}>
          <MemoryRouter
            initialEntries={["/u/test/workspaces/ws/files/docs/a%20%23%3F.txt"]}
          >
            <Routes>
              <Route
                path="/u/:slug/workspaces/:workspaceId/files/*"
                element={<WorkspacesPage admin={false} />}
              />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      ),
    );
    await act(settle);
    await act(async () => {
      const editor = container.querySelector("textarea")!;
      Object.getOwnPropertyDescriptor(
        HTMLTextAreaElement.prototype,
        "value",
      )!.set!.call(editor, "unsaved edit");
      editor.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await menu(
      kind === "file" ? "File actions: docs/a #?.txt" : "Folder actions: docs",
    );
    await click(item("Rename…"));
    await folderName(kind === "file" ? "renamed #?.txt" : "renamed #?");
    await click(button("Rename"));
    expect(container.querySelector("textarea")?.value).toBe("unsaved edit");
    expect(
      container
        .querySelector('[role="treeitem"][aria-selected="true"]')
        ?.getAttribute("data-tree-path"),
    ).toBe(destination);
    await click(button("Save"));
    expect(mocks.api).toHaveBeenCalledWith(
      "PUT",
      `/api/v1/universes/u/workspaces/ws/files/${destination.split("/").map(encodeURIComponent).join("/")}`,
      {
        contentText: "unsaved edit",
        expectedRevision: 4,
        mediaType: "text/plain",
      },
    );
    await act(async () => {
      const editor = container.querySelector("textarea")!;
      Object.getOwnPropertyDescriptor(
        HTMLTextAreaElement.prototype,
        "value",
      )!.set!.call(editor, "another edit");
      editor.dispatchEvent(new Event("input", { bubbles: true }));
    });
    // Ordinary navigation must still reset drafts, even for identical blobs.
    await click(
      container.querySelector(
        'a[href="/u/test/workspaces/ws/files/other.txt"]',
      ),
    );
    await vi.waitFor(async () => {
      await act(settle);
      expect(container.querySelector("textarea")?.value).toBe("x");
    });
  },
);

it.each(["metaKey", "ctrlKey"] as const)(
  "saves the focused editor with %s+S and suppresses browser Save",
  async (modifier) => {
    const workspace = {
      workspaceId: "ws",
      displayName: "Documents",
      revision: 3,
      files: 1,
    };
    mocks.api.mockImplementation(async (_method: string, path: string) => {
      if (path.endsWith("/workspaces")) return [workspace];
      if (path.endsWith("/tree")) return { ...tree(), workspace };
      if (path.includes("/files/")) return { bytesBase64: "eA==", bytes: 1 };
      throw new Error(`Unexpected request: ${path}`);
    });
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    });
    await act(async () =>
      root.render(
        <QueryClientProvider client={client}>
          <MemoryRouter
            initialEntries={["/u/test/workspaces/ws/files/docs/a%20%23%3F.txt"]}
          >
            <Routes>
              <Route
                path="/u/:slug/workspaces/:workspaceId/files/*"
                element={<WorkspacesPage admin={false} />}
              />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      ),
    );
    await act(settle);
    const editor = container.querySelector("textarea")!;
    const pressSave = () => {
      const event = new KeyboardEvent("keydown", {
        key: "s",
        [modifier]: true,
        bubbles: true,
        cancelable: true,
      });
      editor.dispatchEvent(event);
      return event;
    };
    await act(async () => {
      editor.focus();
      expect(pressSave().defaultPrevented).toBe(true);
    });
    expect(
      mocks.api.mock.calls.filter(([method]) => method === "PUT"),
    ).toHaveLength(0);
    await act(async () => {
      Object.getOwnPropertyDescriptor(
        HTMLTextAreaElement.prototype,
        "value",
      )!.set!.call(editor, "edited");
      editor.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await act(async () => {
      expect(pressSave().defaultPrevented).toBe(true);
      await settle();
    });
    expect(mocks.api).toHaveBeenCalledWith(
      "PUT",
      "/api/v1/universes/u/workspaces/ws/files/docs/a%20%23%3F.txt",
      { contentText: "edited", expectedRevision: 3, mediaType: "text/plain" },
    );
  },
);

it.each([
  ["report.PDF", undefined, "PDF payload"],
  ["document", "Application/PDF; version=1.7", "PDF payload"],
  ["document.bin", "application/octet-stream", "%PDF-1.7\n"],
])(
  "opens %s as a PDF instead of editable text",
  async (name, mediaType, content) => {
    Object.defineProperty(navigator, "pdfViewerEnabled", {
      configurable: true,
      value: true,
    });
    URL.createObjectURL = vi.fn(() => "blob:workspace-pdf");
    URL.revokeObjectURL = vi.fn();
    entries = {
      [name]: { ...file, ...(mediaType ? { media_type: mediaType } : {}) },
    };
    const workspace = { workspaceId: "ws", revision: 3, files: 1 };
    mocks.api.mockImplementation(async (_method: string, path: string) => {
      if (path.endsWith("/workspaces")) return [workspace];
      if (path.endsWith("/tree")) return { ...tree(), workspace };
      if (path.includes("/files/"))
        return { bytesBase64: btoa(content), bytes: content.length };
      throw new Error(`Unexpected request: ${path}`);
    });
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    });
    await act(async () =>
      root.render(
        <QueryClientProvider client={client}>
          <MemoryRouter
            initialEntries={[`/u/test/workspaces/ws/files/${name}`]}
          >
            <Routes>
              <Route
                path="/u/:slug/workspaces/:workspaceId/files/*"
                element={<WorkspacesPage admin={false} />}
              />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      ),
    );
    await vi.waitFor(async () => {
      await act(settle);
      expect(container.querySelector("iframe")?.getAttribute("src")).toBe(
        "blob:workspace-pdf",
      );
    });
    expect(container.querySelector("textarea")).toBeNull();
    expect(button("Save")).toBeUndefined();
    expect(button("Saved")).toBeUndefined();
  },
);

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

it.each([
  ["button", false],
  ["metaKey", true],
  ["ctrlKey", false],
] as const)(
  "keeps the editor stable through a delayed %s save and refresh",
  async (trigger, keepTyping) => {
    const write = deferred<unknown>();
    const refreshedTree = deferred<unknown>();
    const refreshedBlob = deferred<unknown>();
    let saved = false;
    let readingNewBlob = false;
    const workspace = {
      workspaceId: "ws",
      displayName: "Documents",
      revision: 3,
      files: 1,
    };
    mocks.api.mockImplementation(async (method: string, path: string) => {
      if (method === "PUT") return write.promise;
      if (path.endsWith("/workspaces")) return [workspace];
      if (path.endsWith("/tree"))
        return saved ? refreshedTree.promise : { ...tree(), workspace };
      if (path.includes("/files/"))
        return readingNewBlob
          ? refreshedBlob.promise
          : { blobRef: "old", bytesBase64: "eA==", bytes: 1 };
      throw new Error(`Unexpected request: ${path}`);
    });
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    });
    await act(async () =>
      root.render(
        <QueryClientProvider client={client}>
          <MemoryRouter
            initialEntries={["/u/test/workspaces/ws/files/docs/a%20%23%3F.txt"]}
          >
            <Routes>
              <Route
                path="/u/:slug/workspaces/:workspaceId/files/*"
                element={<WorkspacesPage admin={false} />}
              />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      ),
    );
    await act(settle);
    const editor = container.querySelector("textarea")!;
    const edit = async (value: string) =>
      act(async () => {
        editor.focus();
        Object.getOwnPropertyDescriptor(
          HTMLTextAreaElement.prototype,
          "value",
        )!.set!.call(editor, value);
        editor.dispatchEvent(new Event("input", { bubbles: true }));
        editor.setSelectionRange(2, 5);
        editor.scrollTop = 80;
      });
    await edit("saved content");
    await act(async () => {
      if (trigger === "button") button("Save")!.click();
      else
        editor.dispatchEvent(
          new KeyboardEvent("keydown", {
            key: "s",
            [trigger]: true,
            bubbles: true,
            cancelable: true,
          }),
        );
      await settle();
    });
    expect(button("Saving…")).toBeDefined();
    if (keepTyping) await edit("newer unsaved content");
    const stable = () => {
      expect(container.querySelector("textarea")).toBe(editor);
      expect(editor.value).toBe(
        keepTyping ? "newer unsaved content" : "saved content",
      );
      expect(document.activeElement).toBe(editor);
      expect([
        editor.selectionStart,
        editor.selectionEnd,
        editor.scrollTop,
      ]).toEqual([2, 5, 80]);
    };
    await act(async () => {
      saved = true;
      write.resolve({ workspace: { ...workspace, revision: 4 } });
      await settle();
    });
    stable();
    await act(async () => {
      readingNewBlob = true;
      entries = {
        docs: {
          kind: "directory",
          entries: {
            "a #?.txt": { ...file, blob_ref: "saved", size_bytes: 13 },
          },
        },
      };
      refreshedTree.resolve({
        ...tree(4),
        workspace: { ...workspace, revision: 4 },
      });
      await settle();
    });
    await act(settle);
    stable();
    expect(
      mocks.api.mock.calls.filter(
        ([method, path]) => method === "GET" && path.includes("/files/"),
      ),
    ).toHaveLength(2);
    await act(async () => {
      refreshedBlob.resolve({
        blobRef: "saved",
        bytesBase64: btoa("saved content"),
        bytes: 13,
      });
      await settle();
    });
    stable();
    expect(button(keepTyping ? "Save" : "Saved")).toBeDefined();
    expect(
      (button(keepTyping ? "Save" : "Saved") as HTMLButtonElement).disabled,
    ).toBe(!keepTyping);
  },
);
