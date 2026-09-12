// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { TranscriptMedia } from "@/lib/sessions/transcript";
import { MarkdownContent } from "./markdown-content";
import { MediaThumb } from "./media";
import { TranscriptLinksContext, type TranscriptLinks } from "./transcript-links";

const image: TranscriptMedia = {
  handle: "media:012345abcdef", blobRef: "sha256:image", mime: "image/webp", kind: "image",
};
const pdf: TranscriptMedia = {
  handle: "media:abcdef012345", blobRef: "sha256:pdf", mime: "application/pdf", kind: "document",
};
const createObjectURL = vi.fn<(blob: Blob) => string>();
const revokeObjectURL = vi.fn<(url: string) => void>();
let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  let nextUrl = 0;
  createObjectURL.mockReset().mockImplementation(() => `blob:http://localhost/media-${++nextUrl}`);
  revokeObjectURL.mockReset();
  vi.stubGlobal("URL", class extends URL {
    static createObjectURL = createObjectURL;
    static revokeObjectURL = revokeObjectURL;
  });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

async function show(loadMedia: NonNullable<TranscriptLinks["loadMedia"]>, children: ReactNode) {
  await act(async () => root.render(
    <TranscriptLinksContext.Provider value={{
      loadMedia, mediaByHandle: new Map([[image.handle, image], [pdf.handle, pdf]]),
    }}>
      {children}
    </TranscriptLinksContext.Provider>,
  ));
}

describe("media links", () => {
  it.each([
    ["tool thumbnail", image, <MediaThumb media={image} />],
    ["markdown image", image, <MarkdownContent>{`![render](${image.handle})`}</MarkdownContent>],
    ["markdown image link", image, <MarkdownContent>{`[open](${image.handle})`}</MarkdownContent>],
    ["document link", pdf, <MarkdownContent>{`[report](${pdf.handle})`}</MarkdownContent>],
  ] as const)("gives a %s a navigable URL and releases it on removal", async (_label, media, content) => {
    const blob = new Blob(["media bytes"], { type: media.mime });
    const loadMedia = vi.fn().mockResolvedValue(blob);
    await show(loadMedia, content);

    const link = container.querySelector<HTMLAnchorElement>("a")!;
    expect(link.href).toBe("blob:http://localhost/media-1");
    expect(link.target).toBe("_blank");
    expect(createObjectURL).toHaveBeenCalledWith(blob);
    expect(loadMedia).toHaveBeenCalledWith(media.blobRef, media.mime);
    if (media.kind === "image") expect(link.querySelector("img")?.src).toBe(link.href);
    expect(revokeObjectURL).not.toHaveBeenCalled();

    await show(loadMedia, null);
    expect(revokeObjectURL).toHaveBeenCalledWith(link.href);
  });

  it("shares a read while keeping each mounted image's URL alive independently", async () => {
    const loadMedia = vi.fn().mockResolvedValue(new Blob(["image"], { type: image.mime }));
    await show(loadMedia, [<MediaThumb key="first" media={image} />, <MediaThumb key="second" media={image} />]);
    const urls = Array.from(container.querySelectorAll("a"), (link) => link.href);
    expect(loadMedia).toHaveBeenCalledTimes(1);
    expect(new Set(urls).size).toBe(2);

    await show(loadMedia, [<MediaThumb key="second" media={{ ...image }} />]);
    expect(container.querySelector("a")?.href).toBe(urls[1]);
    expect(revokeObjectURL.mock.calls).toEqual([[urls[0]]]);
    expect(createObjectURL).toHaveBeenCalledTimes(2);
  });

  it("does not reuse a previous reader's image while a new reader is loading", async () => {
    const first = vi.fn().mockResolvedValue(new Blob(["first"]));
    await show(first, <MediaThumb media={image} />);
    let finish!: (blob: Blob) => void;
    const second = vi.fn().mockReturnValue(new Promise<Blob>((resolve) => { finish = resolve; }));
    await show(second, <MediaThumb media={image} />);
    expect(second).toHaveBeenCalledTimes(1);
    expect(container.querySelector("a")).toBeNull();
    expect(revokeObjectURL).toHaveBeenCalledWith("blob:http://localhost/media-1");

    await act(async () => finish(new Blob(["second"])));
    expect(container.querySelector("a")?.href).toBe("blob:http://localhost/media-2");
  });

  it("does not create an object URL if the image is removed before its read finishes", async () => {
    let finish!: (blob: Blob) => void;
    const loadMedia = vi.fn().mockReturnValue(new Promise<Blob>((resolve) => { finish = resolve; }));
    await show(loadMedia, <MediaThumb media={image} />);
    await show(loadMedia, null);
    await act(async () => finish(new Blob(["late image"])));
    expect(createObjectURL).not.toHaveBeenCalled();
  });
});
