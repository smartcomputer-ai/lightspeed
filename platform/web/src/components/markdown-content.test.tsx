// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { MarkdownContent } from "./markdown-content";

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

async function render(markdown: string) {
  await act(async () => root.render(<MarkdownContent>{markdown}</MarkdownContent>));
}

it.each([
  ["https", "![chart](https://images.example/chart.png?session=secret)", "https://images.example/chart.png?session=secret", "chart (images.example)"],
  ["http with a port", "![chart](http://images.example:8080/chart.png)", "http://images.example:8080/chart.png", "chart (images.example:8080)"],
  ["no alt text", "![](https://images.example/pixel.gif)", "https://images.example/pixel.gif", "Image (images.example)"],
  ["a titled image", '![chart](https://images.example/chart.png "Quarterly")', "https://images.example/chart.png", "chart (images.example)"],
  ["a scheme-relative address", "![chart](//images.example/chart.png)", "//images.example/chart.png", "chart (images.example)"],
  ["a reference definition", "![chart][c]\n\n[c]: https://images.example/chart.png", "https://images.example/chart.png", "chart (images.example)"],
])("renders a remote image as a link naming its host, never an image: %s", async (_case, markdown, href, text) => {
  await render(markdown);
  expect(container.querySelector("img")).toBeNull();
  const link = container.querySelector("a")!;
  expect(link.getAttribute("href")).toBe(href);
  expect(link.textContent).toBe(text);
  expect(link.target).toBe("_blank");
  expect(link.rel).toBe("noreferrer");
});

it("names a linked image's host inside the link that surrounds it", async () => {
  await render("[![badge](https://images.example/badge.svg)](https://project.example/)");
  expect(container.querySelector("img")).toBeNull();
  const links = [...container.querySelectorAll("a")];
  expect(links.map((link) => link.getAttribute("href"))).toEqual(["https://project.example/"]);
  expect(links[0]!.textContent).toBe("badge (images.example)");
});

it("links an image on this origin by its alt text alone", async () => {
  await render("![diagram](/static/diagram.png)");
  expect(container.querySelector("img")).toBeNull();
  const link = container.querySelector("a")!;
  expect(link.getAttribute("href")).toBe("/static/diagram.png");
  expect(link.textContent).toBe("diagram");
});

it.each([
  "![inline](data:image/png;base64,iVBORw0KGgo=)",
  "![script](javascript:alert(1))",
])("shows only the alt text of an image whose address is not allowed: %s", async (markdown) => {
  await render(markdown);
  expect(container.querySelector("img")).toBeNull();
  expect(container.querySelector("a")).toBeNull();
  expect(container.textContent).toMatch(/inline|script/);
});

it("does not turn raw HTML into an image", async () => {
  await render('<img src="https://images.example/pixel.gif" alt="pixel">');
  expect(container.querySelector("img")).toBeNull();
});

it("leaves references without an attachment resolver inert", async () => {
  await render("[workspace](file:ffffffffffffffffffffffff) and ![image](media:012345abcdef)");
  expect(container.querySelector("a, img")).toBeNull();
  expect(container.textContent).toContain("workspace (unavailable)");
  expect(container.textContent).toContain("image (not found)");
});
