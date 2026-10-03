// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { appHref, blobHref } from "@/lib/blob-view";
import { ToolGroupTrace } from "./tool-trace";
import { MarkdownContent } from "./markdown-content";
import { TranscriptLinksContext } from "./transcript-links";
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
async function render(text: string, references = [file]) {
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
it("renders image syntax as a file link without loading the custom protocol", async () => {
  await render(`![report](${file.handle})`);
  expect(container.querySelector("img")).toBeNull();
  expect(container.querySelector("a")!.textContent).toBe("report");
});
it("continues to sanitize unsafe links", async () => {
  await render("[bad](javascript:alert%281%29)");
  expect(container.querySelector("a")!.getAttribute("href")).not.toContain(
    "javascript:",
  );
});

it("resolves a reference outside the loaded history and scopes the lookup to its session", async () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const load = vi.fn(async () => file);
  await act(async () =>
    root.render(
      <QueryClientProvider client={client}>
        <TranscriptLinksContext.Provider
          value={{
            fileReferenceSource: { universeId: "u", sessionId: "s1", load },
            blobHref: (ref, hints) =>
              blobHref("acme", ref, { ...hints, session: "s1" }),
          }}
        >
          <MarkdownContent>{`[report](${file.handle})`}</MarkdownContent>
        </TranscriptLinksContext.Provider>
      </QueryClientProvider>,
    ),
  );
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 20));
  });
  expect(load).toHaveBeenCalledWith(file.handle, expect.any(AbortSignal));
  expect(container.querySelector("a")!.href).toContain("session=s1");
  expect(
    client.getQueryData(["file-reference", "u", "s1", file.handle, undefined]),
  ).toEqual(file);
  expect(
    client.getQueryData(["file-reference", "u", "s2", file.handle, undefined]),
  ).toBeUndefined();
});


it("checks history even for a visible reference and refreshes when new metadata arrives", async () => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
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
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 10)); });
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
