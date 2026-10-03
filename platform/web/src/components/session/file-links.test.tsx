// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { waitForUi } from "@/test/wait-for-ui";
import { appHref, blobHref } from "@/lib/blob-view";
import { ToolGroupTrace } from "./tool-trace";
import { MarkdownContent } from "./markdown-content";
import { TranscriptLinksContext, type TranscriptLinks } from "./transcript-links";
import type { FileReference } from "@/lib/file-references";

const file: FileReference = {
  handle: `file:${"a".repeat(24)}`,
  blobRef: `sha256:${"b".repeat(64)}`,
  name: "résumé #?.txt",
  path: "docs/résumé #?.txt",
  workspace: "ws",
  type: "text/plain",
};
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  client.clear();
  container.remove();
  vi.unstubAllGlobals();
});
async function render(text: string, references = [file], extra: TranscriptLinks = {}) {
  await act(async () =>
    root.render(
      <TranscriptLinksContext.Provider
        value={{
          filesByHandle: new Map(
            references.map((reference) => [reference.handle, reference]),
          ),
          blobHref: (ref, hints) => {
            const path = blobHref("acme", ref, { ...hints, session: "s1" });
            return path && appHref(path);
          },
          ...extra,
        }}
      >
        <MarkdownContent>{text}</MarkdownContent>
      </TranscriptLinksContext.Provider>,
    ),
  );
}
it("opens the exact file blob in a new tab with its session and workspace origins", async () => {
  await render(`Here is the [report](${file.handle}).`);
  const anchor = container.querySelector("a")!;
  const url = new URL(anchor.href);
  expect(url.pathname).toBe(
    `${import.meta.env.BASE_URL.replace(/\/$/, "")}/u/acme/blobs/${"b".repeat(64)}`,
  );
  expect(Object.fromEntries(url.searchParams)).toEqual({
    name: "résumé #?.txt",
    type: "text/plain",
    session: "s1",
    workspace: "ws",
    path: file.path,
  });
  expect(anchor.textContent).toBe("report");
  expect(anchor.target).toBe("_blank");
  expect(anchor.rel).toBe("noopener noreferrer");
});
it("opens snapshot file versions without a workspace backlink", async () => {
  await render(`[report](${file.handle})`, [{ ...file, workspace: undefined }]);
  const url = new URL(container.querySelector("a")!.href);
  expect(url.searchParams.get("workspace")).toBeNull();
  expect(url.searchParams.get("session")).toBe("s1");
});
it.each([`file:${"c".repeat(24)}`, "file:bad", "file://evil.example/file"])(
  "renders unavailable references without custom-protocol navigation: %s",
  async (handle) => {
    await render(`[report](${handle})`);
    expect(container.querySelector("a")).toBeNull();
    expect(container.textContent).toContain("report (unavailable)");
  },
);
it("falls back to a file link for image syntax targeting a text file", async () => {
  const loadMedia = vi.fn();
  await render(`![report](${file.handle})`, [file], { loadMedia });
  expect(container.querySelector("img")).toBeNull();
  expect(container.querySelector("a")!.textContent).toBe("report");
  expect(loadMedia).not.toHaveBeenCalled();
});
it("keeps ordinary links to image files as text without loading the image", async () => {
  const loadMedia = vi.fn();
  await render(`[**open image**](${file.handle})`, [{ ...file, type: "image/jpeg" }], { loadMedia });
  expect(container.querySelector("a strong")?.textContent).toBe("open image");
  expect(container.querySelector("img")).toBeNull();
  expect(loadMedia).not.toHaveBeenCalled();
});
it.each(["image/jpeg", undefined])("renders image syntax for a file with type %s and preserves its origin", async (type) => {
  const createObjectURL = vi.fn(() => "blob:http://localhost/image");
  const revokeObjectURL = vi.fn();
  vi.stubGlobal("URL", class extends URL {
    static createObjectURL = createObjectURL;
    static revokeObjectURL = revokeObjectURL;
  });
  const loadMedia = vi.fn().mockResolvedValue(new Blob(["image bytes"]));
  await render(`![A cartoon](${file.handle})`, [{ ...file, type }], { loadMedia });
  const img = container.querySelector("img")!;
  expect(img.src).toBe("blob:http://localhost/image");
  expect(img.alt).toBe("A cartoon");
  const target = new URL(img.closest("a")!.href);
  expect(target.pathname).toContain(`/blobs/${"b".repeat(64)}`);
  expect(target.searchParams.get("session")).toBe("s1");
  expect(target.searchParams.get("workspace")).toBe("ws");
  expect(target.searchParams.get("path")).toBe(file.path);
  expect(loadMedia).toHaveBeenCalledWith(file.blobRef, type ?? "application/octet-stream");
  await render("removed");
  expect(revokeObjectURL).toHaveBeenCalledWith("blob:http://localhost/image");
});
it("falls back to the file link when bytes cannot be displayed as an image", async () => {
  vi.stubGlobal("URL", class extends URL {
    static createObjectURL = () => "blob:http://localhost/not-an-image";
    static revokeObjectURL = vi.fn();
  });
  const loadMedia = vi.fn().mockResolvedValue(new Blob(["not image bytes"]));
  await render(`![report](${file.handle})`, [{ ...file, type: undefined }], { loadMedia });
  await act(async () => container.querySelector("img")!.dispatchEvent(new Event("error")));
  expect(container.querySelector("img")).toBeNull();
  expect(container.querySelector("a")!.textContent).toBe("report");
  expect(container.querySelector("a")!.href).toContain(`/blobs/${"b".repeat(64)}`);
});
it("continues to sanitize unsafe links", async () => {
  await render("[bad](javascript:alert%281%29)");
  expect(container.querySelector("a")!.getAttribute("href")).not.toContain(
    "javascript:",
  );
});

it.each([false, true])("resolves a reference outside the loaded history and scopes the lookup to its session (image: %s)", async (showImage) => {
  const resolved = showImage ? { ...file, type: "image/jpeg" } : file;
  const load = vi.fn(async () => resolved);
  vi.stubGlobal("URL", class extends URL {
    static createObjectURL = () => "blob:http://localhost/historical-image";
    static revokeObjectURL = vi.fn();
  });
  await act(async () =>
    root.render(
      <QueryClientProvider client={client}>
        <TranscriptLinksContext.Provider
          value={{
            fileReferenceSource: { universeId: "u", sessionId: "s1", load },
            loadMedia: async () => new Blob(["image bytes"]),
            blobHref: (ref, hints) =>
              blobHref("acme", ref, { ...hints, session: "s1" }),
          }}
        >
          <MarkdownContent>{`${showImage ? "!" : ""}[report](${file.handle})`}</MarkdownContent>
        </TranscriptLinksContext.Provider>
      </QueryClientProvider>,
    ),
  );
  await waitForUi(() => {
    expect(container.querySelector("a")?.href).toContain("session=s1");
    expect(container.querySelector("img")?.getAttribute("src")).toBe(showImage ? "blob:http://localhost/historical-image" : undefined);
  });
  expect(load).toHaveBeenCalledWith(file.handle, expect.any(AbortSignal));
  expect(container.querySelector("a")!.href).toContain("session=s1");
  expect(container.querySelector("img")?.getAttribute("src")).toBe(showImage ? "blob:http://localhost/historical-image" : undefined);
  expect(
    client.getQueryData(["file-reference", "u", "s1", file.handle, undefined]),
  ).toEqual(resolved);
  expect(
    client.getQueryData(["file-reference", "u", "s2", file.handle, undefined]),
  ).toBeUndefined();
});


it("checks history even for a visible reference and refreshes when new metadata arrives", async () => {
  const load = vi.fn().mockResolvedValueOnce(null).mockResolvedValueOnce(file);
  const show = async (known: boolean) => {
    await act(async () => root.render(
      <QueryClientProvider client={client}>
        <TranscriptLinksContext.Provider value={{
          filesByHandle: new Map(known ? [[file.handle, file]] : []),
          fileReferenceSource: { universeId: "u", sessionId: "s", load },
          blobHref: (ref, hints) => blobHref("acme", ref, hints),
        }}>
          <MarkdownContent>{`[report](${file.handle})`}</MarkdownContent>
        </TranscriptLinksContext.Provider>
      </QueryClientProvider>,
    ));
    await waitForUi(() => {
      expect(load).toHaveBeenCalledTimes(known ? 2 : 1);
      expect(client.isFetching()).toBe(0);
      expect(client.getQueryCache().getAll().at(-1)?.state.status).toBe("success");
    });
  };
  await show(false);
  expect(container.querySelector("a")).toBeNull();
  await show(true);
  expect(load).toHaveBeenCalledTimes(2);
  expect(container.querySelector("a")?.textContent).toBe("report");
  client.clear();
});

it("shows file attachments beside tool output without an Effects tab", async () => {
  const digest = "b".repeat(64);
  await act(async () => root.render(
    <TranscriptLinksContext.Provider value={{ blobHref: (ref, hints) => blobHref("acme", ref, hints) }}>
      <ToolGroupTrace group={{ kind: "tool-group", key: "g", status: "succeeded", calls: [{
        callId: "reference", toolName: "vfs_reference", status: "succeeded", isError: false,
        output: "File attachment available", attachments: [{
          kind: "file", handle: `file:${digest.slice(0, 24)}`, contentRef: `sha256:${digest}`, name: "report.md",
        }],
      }] }} />
    </TranscriptLinksContext.Provider>,
  ));
  await act(async () => container.querySelector<HTMLButtonElement>("button[aria-expanded]")!.click());
  expect(container.querySelector("a")?.textContent).toBe("report.md");
  expect(container.querySelector("a")?.href).toContain(`/blobs/${digest}`);
  expect(container.textContent).toContain("File attachment available");
  expect(container.textContent).not.toContain("Effects");
});
