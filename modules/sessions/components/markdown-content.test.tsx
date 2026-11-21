// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { TranscriptMedia } from "./model/transcript";
import { MarkdownContent } from "./markdown-content";
import { TranscriptLinksContext } from "./transcript-links";

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

it("still displays an image recorded with the session", async () => {
  const media: TranscriptMedia = { handle: "media:012345abcdef", blobRef: "sha256:image", mime: "image/png", kind: "image" };
  vi.stubGlobal("URL", class extends URL {
    static createObjectURL = () => "blob:http://localhost/recorded";
    static revokeObjectURL = () => undefined;
  });
  const loadMedia = vi.fn().mockResolvedValue(new Blob(["bytes"], { type: media.mime }));
  await act(async () => root.render(
    <TranscriptLinksContext.Provider value={{ loadMedia, mediaByHandle: new Map([[media.handle, media]]) }}>
      <MarkdownContent>{`![render](${media.handle}) and ![remote](https://images.example/a.png)`}</MarkdownContent>
    </TranscriptLinksContext.Provider>,
  ));
  const images = [...container.querySelectorAll("img")];
  expect(images.map((image) => image.getAttribute("src"))).toEqual(["blob:http://localhost/recorded"]);
  expect(container.textContent).toContain("remote (images.example)");
});
