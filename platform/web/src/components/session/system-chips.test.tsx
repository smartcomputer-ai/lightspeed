// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { blobHref } from "@/lib/blob-view";
import { TranscriptLinksContext, type TranscriptLinks } from "./transcript-links";
import { SystemChips } from "./transcript-view";

const digest = "a".repeat(64);
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

async function show(links: TranscriptLinks) {
  await act(async () => root.render(
    <TranscriptLinksContext.Provider value={links}>
      <SystemChips entries={[
        { kind: "system", key: "a", text: "Profile instructions", contentRef: `sha256:${digest}` },
        { kind: "system", key: "b", text: "Legacy" },
      ]} />
    </TranscriptLinksContext.Provider>,
  ));
}

it("links a chip with stored text to its blob page in a new tab", async () => {
  await show({ blobHref: (ref, hints) => blobHref("acme", ref, { ...hints, session: "s1" }) });
  const links = container.querySelectorAll("a");
  expect(links).toHaveLength(1);
  expect(links[0]!.getAttribute("href")).toBe(`/u/acme/blobs/${digest}?name=Profile+instructions&type=text%2Fmarkdown&session=s1`);
  expect(links[0]!.target).toBe("_blank");
  expect(container.textContent).toBe("Profile instructionsLegacy");
});

it("renders plain labels outside a page that links blobs", async () => {
  await show({});
  expect(container.querySelector("a")).toBeNull();
});
