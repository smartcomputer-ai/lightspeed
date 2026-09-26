// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ResizeHandle, clampWidth, useResizableWidth } from "./resize-handle";

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

function Column({ stored, onCommit, room }: { stored: number | null; onCommit: (width: number | null) => void; room?: () => number }) {
  const resizable = useResizableWidth({ stored, fallback: 320, min: 256, max: 576, room, onCommit });
  return (
    <div data-width={resizable.width} data-resizing={resizable.resizing}>
      <ResizeHandle label="Resize list" handle={resizable.handle} />
    </div>
  );
}

async function render(props: Parameters<typeof Column>[0]) {
  await act(async () => root.render(<Column {...props} />));
  return container.querySelector<HTMLDivElement>('[role="separator"]')!;
}

const width = () => Number(container.querySelector<HTMLElement>("[data-width]")!.dataset.width);

function pointer(target: HTMLElement, type: string, clientX: number) {
  target.dispatchEvent(new MouseEvent(type, { bubbles: true, button: 0, clientX }));
}

it("keeps widths within their bounds", () => {
  expect(clampWidth(100, 256, 576)).toBe(256);
  expect(clampWidth(900, 256, 576)).toBe(576);
  expect(clampWidth(300.4, 256, 576)).toBe(300);
  // No room at all still leaves the minimum.
  expect(clampWidth(300, 256, 100)).toBe(256);
});

it("uses the default until a width is stored, and a stored one out of bounds is clamped", async () => {
  await render({ stored: null, onCommit: () => undefined });
  expect(width()).toBe(320);
  await render({ stored: 2000, onCommit: () => undefined });
  expect(width()).toBe(576);
});

it("follows a drag live and commits once, within the room the page has", async () => {
  const onCommit = vi.fn();
  const handle = await render({ stored: 320, onCommit, room: () => 400 });
  await act(async () => pointer(handle, "pointerdown", 500));
  await act(async () => pointer(handle, "pointermove", 540));
  expect(width()).toBe(360);
  expect(container.querySelector<HTMLElement>("[data-width]")!.dataset.resizing).toBe("true");
  await act(async () => pointer(handle, "pointermove", 700));
  expect(width()).toBe(400);
  expect(onCommit).not.toHaveBeenCalled();
  await act(async () => pointer(handle, "pointerup", 700));
  expect(onCommit).toHaveBeenCalledExactlyOnceWith(400);
  expect(document.body.style.cursor).toBe("");
});

it("resets on double-click and steps with the arrow keys", async () => {
  const onCommit = vi.fn();
  const handle = await render({ stored: 320, onCommit });
  await act(async () => handle.dispatchEvent(new MouseEvent("dblclick", { bubbles: true })));
  expect(onCommit).toHaveBeenLastCalledWith(null);
  await act(async () => handle.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true })));
  expect(onCommit).toHaveBeenLastCalledWith(336);
  await act(async () => handle.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true })));
  expect(onCommit).toHaveBeenLastCalledWith(304);
});

it("ends a drag the browser takes away without a release, and saves it", async () => {
  const onCommit = vi.fn();
  const handle = await render({ stored: 320, onCommit });
  await act(async () => pointer(handle, "pointerdown", 500));
  await act(async () => pointer(handle, "pointermove", 560));
  await act(async () => pointer(handle, "lostpointercapture", 560));
  expect(onCommit).toHaveBeenCalledExactlyOnceWith(380);
  expect(container.querySelector<HTMLElement>("[data-width]")!.dataset.resizing).toBe("false");
  // A release after that is no second commit.
  await act(async () => pointer(handle, "pointerup", 560));
  expect(onCommit).toHaveBeenCalledOnce();
});

it("saves the width a column showed when it goes away mid-drag", async () => {
  const onCommit = vi.fn();
  const handle = await render({ stored: 320, onCommit });
  await act(async () => pointer(handle, "pointerdown", 500));
  await act(async () => pointer(handle, "pointermove", 600));
  await act(async () => root.render(null));
  expect(onCommit).toHaveBeenCalledExactlyOnceWith(420);
  expect(document.body.style.cursor).toBe("");
});

it("does not take two quick drags for a double-click reset", async () => {
  const onCommit = vi.fn();
  const handle = await render({ stored: 320, onCommit });
  await act(async () => pointer(handle, "pointerdown", 500));
  await act(async () => pointer(handle, "pointermove", 520));
  await act(async () => pointer(handle, "pointerup", 520));
  await act(async () => handle.dispatchEvent(new MouseEvent("dblclick", { bubbles: true })));
  expect(onCommit).toHaveBeenCalledExactlyOnceWith(340);
});
