// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PdfPreview } from "./pdf-preview";

let root: Root;
let container: HTMLDivElement;
const originalSupport = Object.getOwnPropertyDescriptor(
  navigator,
  "pdfViewerEnabled",
);
const createUrl = vi.fn();
const revokeUrl = vi.fn();
const bytes = new TextEncoder().encode("%PDF-1.7\n");
const support = (value: boolean | undefined) =>
  Object.defineProperty(navigator, "pdfViewerEnabled", {
    configurable: true,
    value,
  });
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  let sequence = 0;
  createUrl.mockReset().mockImplementation(() => `blob:pdf-${++sequence}`);
  revokeUrl.mockReset();
  vi.stubGlobal(
    "URL",
    class extends URL {
      static createObjectURL = createUrl;
      static revokeObjectURL = revokeUrl;
    },
  );
  support(true);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  if (originalSupport)
    Object.defineProperty(navigator, "pdfViewerEnabled", originalSupport);
  else Reflect.deleteProperty(navigator, "pdfViewerEnabled");
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
async function render(content = bytes, name = "docs/report.pdf") {
  await act(async () =>
    root.render(<PdfPreview bytes={content} name={name} />),
  );
}

it("uses the browser viewer and releases object URLs when content changes or the preview closes", async () => {
  await render();
  const frame = container.querySelector("iframe")!;
  expect(frame.title).toBe("PDF preview: docs/report.pdf");
  expect(frame.getAttribute("src")).toBe("blob:pdf-1");
  const blob = createUrl.mock.calls[0]![0] as Blob;
  expect(blob.type).toBe("application/pdf");
  expect(blob.size).toBe(bytes.length);
  expect(container.querySelector("a")).toBeNull();
  await render(bytes, "docs/renamed.pdf");
  expect(createUrl).toHaveBeenCalledTimes(1);
  expect(container.querySelector("iframe")).toBe(frame);
  await render(new TextEncoder().encode("%PDF-1.7\nnew"));
  expect(revokeUrl).toHaveBeenCalledWith("blob:pdf-1");
  expect(container.querySelector("iframe")?.getAttribute("src")).toBe(
    "blob:pdf-2",
  );
  await act(async () => root.render(null));
  expect(revokeUrl).toHaveBeenCalledWith("blob:pdf-2");
});

it.each([false, undefined])(
  "offers only a manual download when inline support is %s",
  async (enabled) => {
    support(enabled);
    const click = vi
      .spyOn(HTMLAnchorElement.prototype, "click")
      .mockImplementation(() => {});
    await render(bytes, "docs/résumé #1?.pdf");
    expect(container.querySelector("iframe")).toBeNull();
    expect(container.textContent).toContain(
      "doesn’t support inline PDF viewing",
    );
    const link = container.querySelector("a")!;
    expect(link.textContent).toContain("Download PDF");
    expect(link.getAttribute("href")).toBe("blob:pdf-1");
    expect(link.download).toBe("résumé #1?.pdf");
    expect(click).not.toHaveBeenCalled();
  },
);
