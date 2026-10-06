// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, expect, it, vi } from "vitest";
import { SessionConfigEditor } from "./session-config-editor";

let root: Root;
let container: HTMLDivElement;
afterEach(async () => {
  await act(async () => root?.unmount());
  container?.remove();
  vi.unstubAllGlobals();
});

it("keeps read-only settings expandable and copyable while disabling every edit control", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const onChange = vi.fn();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () => root.render(<SessionConfigEditor readOnly onChange={onChange} value={{
    model: { providerId: "openai", apiKind: "openai:responses", model: "test-model" },
    features: {
      environments: { environments: [{ environmentId: "env", access: "exec", default: true }] },
      vfs: { workspaces: [{ workspaceId: "files", path: "/workspace", access: "read" }] },
      mcp: { servers: [{ serverId: "tools", tools: ["search"] }] },
      subagents: { agents: [{ profileId: "helper" }] },
      web: { search: {}, fetch: {} }, timers: {},
    },
  }} />));
  // Open each disclosure once, including those revealed inside another section.
  const opened = new Set<Element>();
  for (let pass = 0; pass < 5; pass++) {
    const disclosures = [...container.querySelectorAll<HTMLButtonElement>('button[aria-expanded="false"]')]
      .filter((button) => !button.disabled && !opened.has(button));
    for (const button of disclosures) {
      opened.add(button);
      await act(async () => button.click());
    }
  }
  expect(container.textContent).toContain("Workspace");
  expect(container.textContent).toContain("search");
  const inputs = [...container.querySelectorAll<HTMLInputElement>("input")]
    .filter((input) => input.type !== "hidden" && input.getAttribute("aria-label") !== "Search MCP tools");
  expect(inputs.length).toBeGreaterThan(10);
  expect(inputs.filter((input) => !input.readOnly && !input.disabled).map((input) => input.outerHTML)).toEqual([]);
  for (const button of container.querySelectorAll<HTMLButtonElement>("button")) {
    if (button.hasAttribute("aria-expanded") && button.getAttribute("role") !== "combobox") continue;
    expect(button.disabled || button.getAttribute("aria-disabled") === "true", button.outerHTML).toBe(true);
    await act(async () => button.click());
  }
  expect(onChange).not.toHaveBeenCalled();
});
