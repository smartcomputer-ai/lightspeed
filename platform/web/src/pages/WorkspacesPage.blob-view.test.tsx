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
    slug: "test",
    isLoading: false,
  }),
}));

const digest = "a".repeat(64);
const name = "résumé #?.txt";
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
let blobRef: string;
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
  blobRef = `sha256:${digest}`;
  mocks.api
    .mockReset()
    .mockImplementation(async (_method: string, path: string) => {
      if (path.endsWith("/workspaces")) return [tree().workspace];
      if (path.endsWith("/tree")) return tree();
      if (path.includes("/files/"))
        return { blobRef, bytesBase64: "eA==", bytes: 1 };
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
async function render() {
  await act(async () =>
    root.render(
      <QueryClientProvider client={client}>
        <MemoryRouter
          initialEntries={[
            `/u/test/workspaces/ws/files/docs/${encodeURIComponent(name)}`,
          ]}
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
  await waitForUi(() => expect(container.querySelector("textarea")).not.toBeNull());
}
const link = () =>
  container.querySelector<HTMLAnchorElement>(
    'header a[aria-label="Open in blob viewer (new tab)"]',
  );

it.each([true, false])(
  "opens the saved blob in a separate tab with filename and type hints (editable: %s)",
  async (editable) => {
    mocks.editable = editable;
    await render();
    const anchor = link()!;
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
  expect(link()!.href).toContain(digest);
  expect(link()!.title).toContain("saved version");
  expect(mocks.api.mock.calls.every(([method]) => method === "GET")).toBe(true);
  const nextDigest = "b".repeat(64);
  await act(async () => {
    blobRef = `sha256:${nextDigest}`;
    client.setQueryData(["workspace-tree", "u", "ws"], tree());
  });
  await waitForUi(() => {
    expect(link()!.href).toContain(nextDigest);
  });
  expect(editor.value).toBe("unsaved edit");
});

it("does not offer a broken link for a malformed blob reference", async () => {
  blobRef = "invalid";
  await render();
  expect(link()).toBeNull();
});
