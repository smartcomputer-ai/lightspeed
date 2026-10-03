// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "./dropdown-menu";
import { Button } from "./button";

let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
  HTMLElement.prototype.scrollIntoView = vi.fn();
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue(
    new DOMRect(0, 0, 120, 32),
  );
  vi.spyOn(HTMLElement.prototype, "getClientRects").mockReturnValue([
    new DOMRect(0, 0, 120, 32),
  ] as unknown as DOMRectList);
  // jsdom has no fullscreen/modal top layer. Its selector engine recursively
  // calls Element.matches for these native states when the popup checks them.
  const matches = Element.prototype.matches;
  vi.spyOn(Element.prototype, "matches").mockImplementation(function (
    this: Element,
    selector: string,
  ) {
    if ([":fullscreen", ":modal", ":popover-open"].includes(selector))
      return false;
    return matches.call(this, selector);
  });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.clearAllTimers();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});
async function key(target: Element, key: string) {
  await act(async () => {
    target.dispatchEvent(
      new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }),
    );
  });
  await act(async () => { await vi.runAllTimersAsync(); });
}
it("opens with the keyboard, moves between actions and restores focus on Escape", async () => {
  const download = vi.fn();
  await act(async () =>
    root.render(
      <DropdownMenu>
        <DropdownMenuTrigger
          render={<Button variant="ghost">File actions</Button>}
        />
        <DropdownMenuContent>
          <DropdownMenuItem>Upload replacement</DropdownMenuItem>
          <DropdownMenuItem onClick={download}>Download</DropdownMenuItem>
          <DropdownMenuItem>Delete</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>,
    ),
  );
  const trigger = container.querySelector("button")!;
  await act(async () => trigger.focus());
  await key(trigger, "ArrowDown");
  expect(document.querySelector('[role="menu"]')).not.toBeNull();
  expect(document.activeElement?.textContent).toBe("Upload replacement");
  await key(document.activeElement!, "ArrowDown");
  expect(document.activeElement?.textContent).toBe("Download");
  await key(document.activeElement!, "Escape");
  expect(document.activeElement).toBe(trigger);
  await act(async () => {
    trigger.dispatchEvent(
      new MouseEvent("click", { bubbles: true, detail: 1 }),
    );
  });
  await act(async () => { await vi.runAllTimersAsync(); });
  expect(document.activeElement?.getAttribute("role")).toBe("menu");
  await key(document.activeElement!, "ArrowDown");
  expect(document.activeElement?.textContent).toBe("Upload replacement");
  await key(document.activeElement!, "ArrowDown");
  expect(document.activeElement?.textContent).toBe("Download");
  await key(document.activeElement!, "Enter");
  expect(download).toHaveBeenCalledOnce();
  expect(document.activeElement).toBe(trigger);
});
