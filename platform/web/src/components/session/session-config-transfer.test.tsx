// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it } from "vitest";
import { SessionConfigEditor } from "./session-config-editor";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

describe("profile and session transfer availability", () => {
  const container = document.createElement("div");
  let root: ReturnType<typeof createRoot> | undefined;

  afterEach(async () => {
    await act(async () => root?.unmount());
    root = undefined;
    container.remove();
  });

  it.each([
    [false, "Enable Environments to also transfer files"],
    [true, "Capture requires workspace edit access and environment read access."],
  ] as const)("explains attachment grants with environments=%s", async (environments, expected) => {
    document.body.append(container);
    root = createRoot(container);
    await act(async () => {
      root!.render(<SessionConfigEditor
        value={{ features: { vfs: { workspaces: [{ workspaceId: "files", path: "/workspace", access: "edit" }] }, ...(environments ? { environments: {} } : {}) } }}
        onChange={() => {}}
      />);
    });
    const expand = Array.from(container.querySelectorAll<HTMLButtonElement>("button[aria-expanded]"))
      .find((button) => button.textContent?.includes("Virtual File System"));
    expect(expand).toBeDefined();
    await act(async () => expand!.click());
    expect(container.textContent).toContain(expected);
  });
});
