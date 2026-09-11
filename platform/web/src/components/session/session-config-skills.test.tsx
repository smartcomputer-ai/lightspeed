// @vitest-environment jsdom
import { act, useState } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it } from "vitest";
import { SessionConfigEditor, type SessionConfig } from "./session-config-editor";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
Object.assign(window, { PointerEvent: MouseEvent });
let root: ReturnType<typeof createRoot>;
let container: HTMLDivElement;
let current: SessionConfig | undefined;
let error: string | null;
afterEach(async () => {
  await act(async () => root?.unmount());
  container?.remove();
});
async function setup(value: SessionConfig) {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  function Harness() {
    const [config, setConfig] = useState<SessionConfig | undefined>(value);
    current = config;
    return <SessionConfigEditor value={config} onChange={setConfig} onValidityChange={(value) => { error = value; }} />;
  }
  await act(async () => root.render(<Harness />));
  for (const button of container.querySelectorAll<HTMLButtonElement>("button[aria-expanded]")) {
    if (button.textContent?.includes("Environments") || button.textContent?.includes("Virtual File System")) {
      await act(async () => button.click());
    }
  }
}
async function toggle(name: string) {
  const button = container.querySelector<HTMLButtonElement>(`[role="switch"][aria-label="${name}"]`);
  expect(button).not.toBeNull();
  await act(async () => button!.click());
}
async function input(label: string, value: string) {
  const fieldLabel = Array.from(container.querySelectorAll("label")).find((item) => item.textContent === label);
  const field = (container.querySelector(`[aria-label="${label}"]`) ?? (fieldLabel && document.getElementById(fieldLabel.htmlFor))) as HTMLInputElement;
  expect(field).not.toBeNull();
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(field, value);
    field.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
it("shares the environment working directory while source overrides remain independent", async () => {
  await setup({ features: { environments: { jobs: true }, vfs: { tools: "edit" } } });
  await input("Environment working directory", "relative");
  expect(error).toContain("absolute");
  await input("Environment working directory", "/project");
  await toggle("Environment skill discovery");
  await toggle("Environment prompt loading");
  expect(error).toBeNull();
  await input("Environment skill roots", "./skills, /team/skills");
  await input("Environment prompt roots", "./prompts");
  expect(current).toMatchObject({ features: { environments: { workingDirectory: "/project", jobs: true, skills: { roots: ["./skills", "/team/skills"] }, prompts: { roots: ["./prompts"] } } } });
  await input("Environment skill roots", "");
  expect(current).toHaveProperty("features.environments.skills", {});
  await toggle("Environment skill discovery");
  expect(current).not.toHaveProperty("features.environments.skills");
  expect(current).toHaveProperty("features.environments.prompts.roots", ["./prompts"]);
  expect(current).toHaveProperty("features.environments.workingDirectory", "/project");
});
it.each([
  ["skills", "VFS skill discovery", "VFS skill roots"],
  ["prompts", "VFS prompt loading", "VFS prompt roots"],
])("enables %s defaults, validates overrides, and restores defaults when cleared", async (key, switchName, label) => {
  await setup({ features: { environments: { skills: {} }, vfs: {
    workspaceLinks: [{ path: "/workspace", access: "readOnly", target: { type: "workspace", workspaceId: "workspace_1" } }],
  } } });
  await toggle(switchName);
  expect(error).toBeNull();
  expect(current).toHaveProperty(`features.vfs.${key}`, {});
  await input(label, "/outside/custom");
  expect(error).toContain("inside workspace links");
  await input(label, "/workspace/custom");
  expect(error).toBeNull();
  expect(current).toHaveProperty(`features.vfs.${key}.roots`, ["/workspace/custom"]);
  await input(label, "");
  expect(error).toBeNull();
  expect(current).toHaveProperty(`features.vfs.${key}`, {});
  expect(container.querySelector(`[aria-label="${switchName}"]`)?.getAttribute("aria-checked")).toBe("true");
  await toggle(switchName);
  expect(error).toBeNull();
  expect(current).not.toHaveProperty(`features.vfs.${key}`);
  expect(current).toHaveProperty("features.environments.skills", {});
});
it("allows both VFS sources to be enabled without links", async () => {
  await setup({ features: { vfs: {} } });
  await toggle("VFS skill discovery");
  await toggle("VFS prompt loading");
  expect(error).toBeNull();
  expect(current).toEqual({ features: { vfs: { skills: {}, prompts: {} } } });
});

it("uses explicit VFS directory settings without inferring /workspace", async () => {
  await setup({features:{vfs:{workspaceLinks:[{path:"/workspace",access:"readOnly",target:{type:"workspace",workspaceId:"workspace_1"}}]}}});
  expect(current).not.toHaveProperty("features.vfs.workingDirectory");
  await input("VFS working directory", "/outside");
  expect(error).toContain("workspace link");
  await input("VFS working directory", "/workspace");
  expect(error).toBeNull();
  await input("VFS working directory", "");
  expect(current).not.toHaveProperty("features.vfs.workingDirectory");
});
