// @vitest-environment jsdom
import { act, useState } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it } from "vitest";
import { SessionConfigEditor, type EnvironmentOption, type SessionConfig, type WorkspaceOption } from "./session-config-editor";

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
async function setup(value: SessionConfig, options: { environments?: EnvironmentOption[]; workspaces?: WorkspaceOption[] } = {}) {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  function Harness() {
    const [config, setConfig] = useState<SessionConfig | undefined>(value);
    current = config;
    return <SessionConfigEditor {...options} value={config} onChange={setConfig} onValidityChange={(value) => { error = value; }} />;
  }
  await act(async () => root.render(<Harness />));
  for (const button of container.querySelectorAll<HTMLButtonElement>("button[aria-expanded]")) {
    if (button.textContent?.includes("Environments") || button.textContent?.includes("Virtual File System") || button.textContent?.includes("Sub-agents") || button.textContent?.includes("Web")) {
      await act(async () => button.click());
    }
  }
}
async function toggle(name: string) {
  const button = container.querySelector<HTMLButtonElement>(`[role="switch"][aria-label="${name}"]`);
  expect(button).not.toBeNull();
  await act(async () => button!.click());
}
async function expand(name: string) {
  const button = container.querySelector<HTMLButtonElement>(`button[aria-label="Configure ${name}"]`);
  expect(button).not.toBeNull();
  expect(button!.getAttribute("aria-expanded")).toBe("false");
  await act(async () => button!.click());
  expect(button!.getAttribute("aria-expanded")).toBe("true");
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
  await setup({ features: { environments: { environments: [{ environmentId: "runner", access: "jobs" }] }, vfs: { workspaces: [] } } });
  await expand("Environment working directory");
  await input("Environment working directory", "relative");
  expect(error).toContain("absolute");
  await input("Environment working directory", "/project");
  await toggle("Environment skill discovery");
  await toggle("Environment prompt loading");
  expect(error).toBeNull();
  await expand("Environment skill discovery");
  await expand("Environment prompt loading");
  await input("Environment skill roots", "./skills, /team/skills");
  await input("Environment prompt roots", "./prompts");
  expect(current).toMatchObject({ features: { environments: { environments: [{ environmentId: "runner", access: "jobs", workingDirectory: "/project" }], skills: { roots: ["./skills", "/team/skills"] }, prompts: { roots: ["./prompts"] } } } });
  await input("Environment skill roots", "");
  expect(current).toHaveProperty("features.environments.skills", {});
  await toggle("Environment skill discovery");
  expect(current).not.toHaveProperty("features.environments.skills");
  expect(current).toHaveProperty("features.environments.prompts.roots", ["./prompts"]);
  expect(current).toHaveProperty("features.environments.environments.0.workingDirectory", "/project");
});
it("keeps a saved environment working directory collapsed and preserves it when toggled", async () => {
  await setup({ features: { environments: { environments: [
    { environmentId: "runner", access: "exec", workingDirectory: "/project" },
  ] } } });
  expect(container.querySelector('[aria-label="Environment working directory"]')).toBeNull();
  expect(container.textContent).toContain("/project");
  const before = structuredClone(current);
  await expand("Environment working directory");
  expect(current).toEqual(before);
  await input("Environment working directory", "/work");
  const collapse = container.querySelector<HTMLButtonElement>('[aria-label="Configure Environment working directory"]')!;
  await act(async () => collapse.click());
  expect(container.querySelector('[aria-label="Environment working directory"]')).toBeNull();
  expect(current).toHaveProperty("features.environments.environments.0.workingDirectory", "/work");
  await expand("Environment working directory");
  await input("Environment working directory", "");
  expect(current).not.toHaveProperty("features.environments.environments.0.workingDirectory");
  expect(error).toBeNull();
});

it("adds environments with jobs access, defaults the first, and enables selection on the second", async () => {
  await setup({ features: { environments: {} } }, {
    environments: ["first", "second", "third"].map((environmentId) => ({ environmentId, status: "ready" })),
  });
  const add = Array.from(container.querySelectorAll<HTMLButtonElement>("button"))
    .find((button) => button.textContent === "Add environment")!;
  await act(async () => add.click());
  expect(current).toHaveProperty("features.environments.environments", [
    { environmentId: "first", access: "jobs", default: true },
  ]);
  expect(current).not.toHaveProperty("features.environments.selection");
  await act(async () => add.click());
  expect(current).toHaveProperty("features.environments.selection", true);
  await act(async () => add.click());
  expect(current).toHaveProperty("features.environments", {
    environments: [
      { environmentId: "first", access: "jobs", default: true },
      { environmentId: "second", access: "jobs" },
      { environmentId: "third", access: "jobs" },
    ],
    selection: true,
  });
  const label = Array.from(container.querySelectorAll("label"))
    .find((label) => label.textContent === "Environment selection tools")!;
  await act(async () => document.getElementById(label.htmlFor)!.click());
  const nextDefault = container.querySelector<HTMLButtonElement>('[aria-label="Default environment 2"]')!;
  await act(async () => nextDefault.click());
  expect(current).not.toHaveProperty("features.environments.selection");
  expect(current).toHaveProperty("features.environments.environments.1.default", true);
  expect(error).toBeNull();
});

it("adds workspaces without changing an existing snapshot attachment", async () => {
  const snapshot = { path: "/archive", access: "read", snapshotRef: `sha256:${"a".repeat(64)}` };
  await setup({ features: { vfs: { workspaces: [snapshot] } } }, {
    workspaces: [{ workspaceId: "files" }],
  });
  const add = Array.from(container.querySelectorAll<HTMLButtonElement>("button"))
    .find((button) => button.textContent === "Add workspace")!;
  await act(async () => add.click());
  await act(async () => add.click());
  expect(current).toHaveProperty("features.vfs.workspaces", [
    snapshot,
    { workspaceId: "files", path: "/workspace", access: "edit" },
    { workspaceId: "files", path: "/workspace-2", access: "edit" },
  ]);
  expect(container.textContent).not.toContain("Target type");
  expect(error).toBeNull();
});
it.each([
  ["skills", "VFS skill discovery", "VFS skill roots"],
  ["prompts", "VFS prompt loading", "VFS prompt roots"],
])("enables %s defaults, validates overrides, and restores defaults when cleared", async (key, switchName, label) => {
  await setup({ features: { environments: { skills: {} }, vfs: {
    workspaces: [{ path: "/workspace", access: "read", workspaceId: "workspace_1"  }],
  } } });
  await toggle(switchName);
  expect(error).toBeNull();
  expect(current).toHaveProperty(`features.vfs.${key}`, {});
  await expand(switchName);
  await input(label, "/outside/custom");
  expect(error).toContain("inside workspace attachments");
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
  expect(current).toEqual({ features: { vfs: { workspaces: [], skills: {}, prompts: {} } } });
});

it("uses explicit VFS directory settings without inferring /workspace", async () => {
  await setup({features:{vfs:{workspaces:[{path:"/workspace",access:"read",workspaceId:"workspace_1"}]}}});
  expect(current).not.toHaveProperty("features.vfs.workingDirectory");
  await input("VFS working directory", "/outside");
  expect(error).toContain("workspace attachment");
  await input("VFS working directory", "/workspace");
  expect(error).toBeNull();
  await input("VFS working directory", "");
  expect(current).not.toHaveProperty("features.vfs.workingDirectory");
});

it.each([
  ["vfs", "skills", "VFS skill discovery"],
  ["vfs", "prompts", "VFS prompt loading"],
  ["environments", "skills", "Environment skill discovery"],
  ["environments", "prompts", "Environment prompt loading"],
])("keeps %s %s defaults compact and exposes root overrides on demand", async (domain, key, name) => {
  await setup({ features: { [domain]: { [key]: {} } } });
  expect(container.querySelector('input[placeholder="Default directories"]')).toBeNull();
  const before = JSON.stringify(current);
  await expand(name);
  expect(container.querySelector('input[placeholder="Default directories"]')).not.toBeNull();
  expect(JSON.stringify(current)).toBe(before);
  await toggle(name);
  expect(container.querySelector('input[placeholder="Default directories"]')).toBeNull();
  await toggle(name);
  expect(container.querySelector('input[placeholder="Default directories"]')).toBeNull();
  expect(current).toHaveProperty(`features.${domain}.${key}`, {});
});
it("summarizes existing overrides while collapsed and preserves them on expansion", async () => {
  await setup({ features: { environments: { skills: { roots: ["./skills", "/team"] } } } });
  expect(container.textContent).toContain("2 custom roots");
  expect(container.querySelector('input[placeholder="Default directories"]')).toBeNull();
  await expand("Environment skill discovery");
  expect((container.querySelector('input[placeholder="Default directories"]') as HTMLInputElement).value).toBe("./skills, /team");
  await input("Environment skill roots", "");
  expect(container.textContent).not.toContain("custom roots");
  expect(current).toHaveProperty("features.environments.skills", {});
});

it("hides sub-agent limits by default and preserves overrides while collapsed", async () => {
  await setup({ features: { subagents: { agents: ["reviewer"], maxDepth: 3, maxDescendants: 24 } } });
  expect(container.textContent).toContain("Agents");
  expect(container.textContent).toContain("2 custom limits");
  expect(container.querySelector('input[type="number"]')).toBeNull();
  const button = container.querySelector<HTMLButtonElement>('[aria-label="Customize sub-agent limits"]')!;
  const before = JSON.stringify(current);
  await act(async () => button.click());
  expect(button.getAttribute("aria-expanded")).toBe("true");
  expect(container.querySelectorAll('input[type="number"]')).toHaveLength(4);
  expect(JSON.stringify(current)).toBe(before);
  await input("Max depth", "4");
  expect(current).toHaveProperty("features.subagents.maxDepth", 4);
  await input("Max descendants", "");
  expect(current).not.toHaveProperty("features.subagents.maxDescendants");
  await act(async () => button.click());
  expect(container.querySelector('input[type="number"]')).toBeNull();
  expect(current).toHaveProperty("features.subagents.maxDepth", 4);
  expect(container.textContent).toContain("1 custom limit");
});

it("hides search domain filters by default and preserves them while collapsed", async () => {
  await setup({ features: { web: { fetch: {}, search: { allowedDomains: ["example.com"] } } } });
  expect(container.textContent).toContain("Search the web");
  expect(container.textContent).toContain("1 allowed");
  expect(container.querySelector('input[placeholder="All domains"]')).toBeNull();
  const button = container.querySelector<HTMLButtonElement>('[aria-label="Customize search domains"]')!;
  const before = JSON.stringify(current);
  await act(async () => button.click());
  expect(button.getAttribute("aria-expanded")).toBe("true");
  expect(JSON.stringify(current)).toBe(before);
  await input("Allowed domains", "example.com, docs.example.com");
  expect(current).toHaveProperty("features.web.search.allowedDomains", ["example.com", "docs.example.com"]);
  await act(async () => button.click());
  expect(container.querySelector('input[placeholder="All domains"]')).toBeNull();
  expect(container.textContent).toContain("2 allowed");
});
it("preserves exclusive domain filter behavior when customized", async () => {
  await setup({ model: { apiKind: "anthropic:messages", providerId: "anthropic", model: "test" }, features: { web: { search: { allowedDomains: ["example.com"] } } } });
  const button = container.querySelector<HTMLButtonElement>('[aria-label="Customize search domains"]')!;
  await act(async () => button.click());
  await input("Blocked domains", "blocked.example.com");
  expect(current).toHaveProperty("features.web.search.blockedDomains", ["blocked.example.com"]);
  expect(current).not.toHaveProperty("features.web.search.allowedDomains");
  await input("Blocked domains", "");
  expect(current).toHaveProperty("features.web.search", {});
});

it("selects a single default without changing access or source grants", async () => {
  await setup({ features: { environments: { environments: [
    { environmentId: "logs", access: "read", default: true },
    { environmentId: "runner", access: "jobs" },
  ], skills: {}, prompts: {} } } });
  const checkbox = container.querySelector<HTMLButtonElement>('[role="checkbox"][aria-label="Default environment 2"]');
  expect(checkbox).not.toBeNull();
  await act(async () => checkbox!.click());
  expect(current).toHaveProperty("features.environments.environments", [
    { environmentId: "logs", access: "read" },
    { environmentId: "runner", access: "jobs", default: true },
  ]);
  expect(current).toHaveProperty("features.environments.skills", {});
  expect(current).toHaveProperty("features.environments.prompts", {});
  expect(error).toBeNull();
});

it("moves a deleted default to the next environment, falling back to the previous row", async () => {
  await setup({ features: { environments: { environments: [
    { environmentId: "logs", access: "read" },
    { environmentId: "runner", access: "jobs", default: true },
    { environmentId: "build", access: "exec", workingDirectory: "/project" },
  ], selection: true, skills: {}, prompts: {} } } });
  const remove = async (index: number) => {
    const buttons = container.querySelectorAll<HTMLButtonElement>('[aria-label="Remove environment attachment"]');
    await act(async () => buttons[index]!.click());
  };
  await remove(1);
  expect(current).toHaveProperty("features.environments.environments", [
    { environmentId: "logs", access: "read" },
    { environmentId: "build", access: "exec", workingDirectory: "/project", default: true },
  ]);
  await remove(1);
  expect(current).toHaveProperty("features.environments.environments", [
    { environmentId: "logs", access: "read", default: true },
  ]);
  await remove(0);
  expect(current).toHaveProperty("features.environments", {
    environments: [], selection: true, skills: {}, prompts: {},
  });
  expect(error).toBeNull();
});

it.each([false, true])("preserves the default choice when removing another environment (default=%s)", async (hasDefault) => {
  const remaining = { environmentId: "runner", access: "jobs", ...(hasDefault ? { default: true } : {}) };
  await setup({ features: { environments: { environments: [
    { environmentId: "logs", access: "read" }, remaining,
  ] } } });
  const remove = container.querySelector<HTMLButtonElement>('[aria-label="Remove environment attachment"]')!;
  await act(async () => remove.click());
  expect(current).toHaveProperty("features.environments.environments", [remaining]);
  expect(error).toBeNull();
});

it("enables environments with an empty attachment list and places selection after discovery", async () => {
  await setup({});
  await toggle("Enable Environments");
  expect(current).toEqual({ features: { environments: {
    environments: [], prompts: {}, skills: {},
  } } });
  const text = container.textContent!;
  expect(text.indexOf("Environment selection tools")).toBeGreaterThan(text.indexOf("Skill discovery"));
  expect(container.textContent).toContain("No environments attached.");
  await toggle("Enable Environments");
  expect(current ?? {}).not.toHaveProperty("features.environments");
});

it("enables VFS with an empty attachment list and source discovery", async () => {
  await setup({});
  await toggle("Enable Virtual File System: Files, Instructions, Skills");
  expect(current).toEqual({ features: { vfs: { workspaces: [], prompts: {}, skills: {} } } });
  expect(container.querySelector('[aria-label="VFS prompt loading"]')?.getAttribute("aria-checked")).toBe("true");
  expect(container.querySelector('[aria-label="VFS skill discovery"]')?.getAttribute("aria-checked")).toBe("true");
  await toggle("Enable Virtual File System: Files, Instructions, Skills");
  expect(current ?? {}).not.toHaveProperty("features.vfs");
});

it("enables Web with search and page fetching", async () => {
  await setup({});
  await toggle("Enable Web");
  expect(current).toEqual({ features: { web: { search: {}, fetch: {} } } });
  await toggle("Enable Web");
  expect(current ?? {}).not.toHaveProperty("features.web");
});
