// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { SharingMark } from "./sharing";

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

it("marks private work with a lock and shared work with people", async () => {
  await act(async () => root.render(<SharingMark access={{ visibility: "restricted" } as never} />));
  expect(container.querySelector('[aria-label="Private"] svg')).not.toBeNull();
  await act(async () => root.render(<SharingMark access={{ visibility: "universe" } as never} />));
  expect(container.querySelector('[aria-label="Shared"] svg')).not.toBeNull();
});

it("marks only shared work in lists", async () => {
  await act(async () => root.render(<SharingMark access={{ visibility: "restricted" } as never} quiet />));
  expect(container.innerHTML).toBe("");
  await act(async () => root.render(<SharingMark access={{ visibility: "universe" } as never} quiet />));
  expect(container.querySelector('[aria-label="Shared"]')).not.toBeNull();
});
