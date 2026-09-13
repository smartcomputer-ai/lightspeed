// @vitest-environment jsdom
import { act, useState } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it, vi } from "vitest";
import { McpToolSubsetField } from "./mcp-tool-subset-field";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
Object.assign(window, { PointerEvent: MouseEvent });
let root: ReturnType<typeof createRoot>;
let container: HTMLDivElement;
afterEach(async () => {
  await act(async () => root?.unmount());
  container?.remove();
});

it("discovers permitted tools and distinguishes no subset from an empty subset", async () => {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  const discover = vi.fn(async () => ["search", "delete"]);
  let selected: string[] | undefined;
  function Harness() {
    const [tools, setTools] = useState<string[]>();
    selected = tools;
    return (
      <McpToolSubsetField
        serverId="catalog"
        allowedTools={["search"]}
        value={tools}
        discoverTools={discover}
        onChange={setTools}
      />
    );
  }
  await act(async () => root.render(<Harness />));
  expect(discover).not.toHaveBeenCalled();
  await act(async () => container.querySelector<HTMLButtonElement>('[role="switch"]')!.click());
  expect(discover).toHaveBeenCalledWith("catalog");
  expect(selected).toEqual([]);
  expect(container.textContent).toContain("search");
  expect(container.textContent).not.toContain("delete");
  await act(async () => container.querySelector<HTMLButtonElement>('[role="checkbox"]')!.click());
  expect(selected).toEqual(["search"]);
  await act(async () => container.querySelector<HTMLButtonElement>('[role="switch"]')!.click());
  expect(selected).toBeUndefined();
});

it("preserves saved tools when discovery fails", async () => {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  const onChange = vi.fn();
  await act(async () =>
    root.render(
      <McpToolSubsetField
        serverId="catalog"
        value={["search"]}
        discoverTools={async () => {
          throw new Error("Server unavailable");
        }}
        onChange={onChange}
      />,
    ),
  );
  expect(container.querySelector('[role="alert"]')?.textContent).toBe("Server unavailable");
  expect(container.querySelector('[role="checkbox"]')?.getAttribute("aria-checked")).toBe("true");
  expect(onChange).not.toHaveBeenCalled();
});
