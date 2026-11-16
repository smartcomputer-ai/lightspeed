// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider, notifyManager } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { waitForUi } from "@/test/wait-for-ui";
import { WorkspacesPage } from "./WorkspacesPage";

const mocks = vi.hoisted(() => ({ api: vi.fn(), editable: true }));
vi.mock("@/api", async (original) => ({
  ...(await original<typeof import("@/api")>()),
  api: mocks.api,
}));
vi.mock("@/lib/permissions", () => ({
  useActionPermissions: () => ({
    can: (action: string) => action === "read" || mocks.editable,
  }),
}));
vi.mock("@/lib/universes", () => ({
  useActiveUniverse: () => ({
    universe: { id: "u" },
    handle: "test",
    isLoading: false,
  }),
}));

const digest = "a".repeat(64);
const name = "résumé #?.txt";
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
let blobRef: string;
let stored: string;
const tree = () => ({
  workspace: {
    workspaceId: "ws",
    displayName: "Documents",
    revision: 3,
    files: 1,
  },
  manifest: {
    root: {
      entries: {
        docs: {
          kind: "directory",
          entries: {
            "other.txt": { kind: "file", blob_ref: blobRef, size_bytes: 1, executable: false },
            "notes.md": { kind: "file", blob_ref: blobRef, size_bytes: 1, executable: false, media_type: "text/markdown" },
            "other.md": { kind: "file", blob_ref: blobRef, size_bytes: 1, executable: false, media_type: "text/markdown" },
            "report.docx": {
              kind: "file",
              blob_ref: blobRef,
              size_bytes: 4,
              executable: false,
              media_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            },
            "image.png": { kind: "file", blob_ref: blobRef, size_bytes: 1, executable: false, media_type: "image/png" },
            [name]: {
              kind: "file",
              blob_ref: blobRef,
              size_bytes: 1,
              executable: false,
              media_type: "text/plain",
            },
          },
        },
      },
    },
    totals: { files: 1, bytes: 1 },
  },
});
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  notifyManager.setNotifyFunction((notify) => act(notify));
  mocks.editable = true;
  // Each file's mode is remembered on this device.
  localStorage.clear();
  blobRef = `sha256:${digest}`;
  stored = "x";
  mocks.api
    .mockReset()
    .mockImplementation(async (method: string, path: string) => {
      if (path.endsWith("/workspaces")) return [tree().workspace];
      if (path.endsWith("/tree")) return tree();
      if (method === "PUT") return { workspace: { ...tree().workspace, revision: 4 } };
      if (path.includes("/blobs/"))
        return { blobRef, bytesBase64: btoa(stored), bytes: stored.length };
      throw new Error(`Unexpected request: ${path}`);
    });
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  client.clear();
  notifyManager.setNotifyFunction((notify) => notify());
  container.remove();
  vi.unstubAllGlobals();
});
/// Opens a file of docs/, by default the plain-text one, and waits until it shows.
async function render(file = name, ready: () => Element | null = () => container.querySelector("textarea")) {
  await act(async () =>
    root.render(
      <QueryClientProvider client={client}>
        <MemoryRouter
          initialEntries={[
            `/u/test/workspaces/ws/files/docs/${encodeURIComponent(file)}`,
          ]}
        >
          <Routes>
            <Route
              path="/u/:handle/workspaces/:workspaceId/files/*"
              element={<WorkspacesPage admin={false} />}
            />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    ),
  );
  await waitForUi(() => expect(ready()).not.toBeNull());
}
const rendered = () => container.querySelector("section h1:not(header h1)");
/// The blob viewer link, in the content hash item's popover (opened first;
/// the popover renders outside the page container).
const link = async () => {
  const item = container.querySelector<HTMLButtonElement>('header [data-meta-item="content"]')!;
  if (item.getAttribute("aria-expanded") !== "true") await act(async () => item.click());
  return openLink();
};
const openLink = () => document.querySelector<HTMLAnchorElement>('a[aria-label="Open in blob viewer (new tab)"]');

it.each([true, false])(
  "opens the saved blob in a separate tab with filename and type hints (editable: %s)",
  async (editable) => {
    mocks.editable = editable;
    await render();
    const anchor = (await link())!;
    const url = new URL(anchor.href);
    expect(url.pathname).toBe(
      `${import.meta.env.BASE_URL.replace(/\/$/, "")}/u/test/blobs/${digest}`,
    );
    expect(url.searchParams.get("name")).toBe(name);
    expect(url.searchParams.get("type")).toBe("text/plain");
    expect(url.searchParams.get("workspace")).toBe("ws");
    expect(url.searchParams.get("path")).toBe(`docs/${name}`);
    expect(anchor.target).toBe("_blank");
    expect(anchor.rel).toContain("noopener");
    expect(anchor.rel).toContain("noreferrer");
    expect(container.querySelector("textarea")!.readOnly).toBe(!editable);
  },
);

it("keeps unsaved edits local and updates the link when the saved blob changes", async () => {
  await render();
  const editor = container.querySelector("textarea")!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      "value",
    )!.set!.call(editor, "unsaved edit");
    editor.dispatchEvent(new Event("input", { bubbles: true }));
  });
  expect((await link())!.href).toContain(digest);
  expect((await link())!.title).toContain("saved version");
  expect(mocks.api.mock.calls.every(([method]) => method === "GET")).toBe(true);
  const nextDigest = "b".repeat(64);
  await act(async () => {
    blobRef = `sha256:${nextDigest}`;
    client.setQueryData(["workspace-tree", "u", "ws"], tree());
  });
  await waitForUi(() => {
    expect(openLink()!.href).toContain(nextDigest);
  });
  expect(editor.value).toBe("unsaved edit");
});

it("does not offer a broken link for a malformed blob reference", async () => {
  blobRef = "invalid";
  await render();
  expect(await link()).toBeNull();
});

/// A button by its visible label (a save button lays out every state's
/// label but shows one).
const button = (label: string) =>
  Array.from(container.querySelectorAll("button")).find((item) =>
    (item.querySelector("[data-save-label]:not([aria-hidden])") ?? item).textContent === label)!;

it.each([true, false])("opens Markdown in its preview, with its source a toggle away (editable: %s)", async (editable) => {
  mocks.editable = editable;
  stored = "# Notes\n\n**Bold** and ~~removed~~\n\n| Name | Value |\n| --- | --- |\n| A | B |";
  await render("notes.md", rendered);
  expect(rendered()?.textContent).toBe("Notes");
  expect(button("Preview").getAttribute("aria-pressed")).toBe("true");
  expect(container.querySelector("strong")?.textContent).toBe("Bold");
  expect(container.querySelector("del")?.textContent).toBe("removed");
  expect(container.querySelector("td")?.textContent).toBe("A");
  expect(container.querySelector("textarea, [contenteditable=true]")).toBeNull();
  await act(async () => button("Source").click());
  expect(container.querySelector("textarea")!.value).toBe(stored);
  expect(container.querySelector("textarea")!.readOnly).toBe(!editable);
  expect(mocks.api.mock.calls.every(([method]) => method === "GET")).toBe(true);
});

it("previews unsaved edits, preserves them in source, and saves the draft from preview", async () => {
  stored = "# Notes";
  await render("notes.md", rendered);
  await act(async () => button("Source").click());
  const editor = container.querySelector("textarea")!;
  const draft = "# Unsaved draft\n\n**Keep this**";
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(editor, draft);
    editor.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await act(async () => button("Preview").click());
  expect(container.querySelector("strong")?.textContent).toBe("Keep this");
  expect(button("Save").disabled).toBe(false);
  expect((await link())!.title).toContain("saved version");
  await act(async () => button("Source").click());
  expect(container.querySelector("textarea")!.value).toBe(draft);
  expect(mocks.api.mock.calls.every(([method]) => method === "GET")).toBe(true);
  await act(async () => button("Preview").click());
  await act(async () => button("Save").click());
  expect(mocks.api).toHaveBeenCalledWith("PUT", "/api/v1/universes/u/workspaces/ws/files/docs/notes.md", {
    contentText: draft, expectedRevision: 3, mediaType: "text/markdown",
  });
});

it("remembers each file's mode on its own", async () => {
  stored = "# Notes";
  await render("notes.md", rendered);
  await act(async () => button("Source").click());
  // Another Markdown file opens in its own (default) mode.
  stored = "# Other file";
  await act(async () => container.querySelector<HTMLAnchorElement>('a[href$="/files/docs/other.md"]')!.click());
  await waitForUi(() => expect(rendered()?.textContent).toBe("Other file"));
  expect(button("Preview").getAttribute("aria-pressed")).toBe("true");
  // Saved: nothing to save, so it ignores presses.
  expect(button("Saved").getAttribute("aria-disabled")).toBe("true");
  // Back on the first, its mode is the one chosen there.
  stored = "# Notes";
  await act(async () => container.querySelector<HTMLAnchorElement>('a[href$="/files/docs/notes.md"]')!.click());
  await waitForUi(() => expect(container.querySelector("textarea")?.value).toBe("# Notes"));
  expect(button("Source").getAttribute("aria-pressed")).toBe("true");
});

it("shows plain text and images in their one mode, without a toggle", async () => {
  stored = "# Not Markdown";
  await render();
  expect(container.querySelector("textarea")!.value).toBe(stored);
  expect(container.querySelector('[aria-label$=" view"]')).toBeNull();
  expect(button("Preview")).toBeUndefined();
  await act(async () => container.querySelector<HTMLAnchorElement>('a[href$="/files/docs/image.png"]')!.click());
  await waitForUi(() => expect(container.querySelector('img[alt="docs/image.png"]')).not.toBeNull());
  expect(container.querySelector('[aria-label$=" view"]')).toBeNull();
  expect(button("Save")).toBeUndefined();
});

it("shows a file without a preview by its details, with a download", async () => {
  stored = "PK\u0003\u0004\u0000\u0000";
  const info = () => [...container.querySelectorAll("p")].find((item) => item.textContent === "There is no preview for this kind of file.") ?? null;
  await render("report.docx", info);
  expect(container.textContent).toContain("report.docx");
  expect(button("Download")).toBeDefined();
  expect(container.querySelector("textarea, img")).toBeNull();
  expect(button("Save")).toBeUndefined();
});
