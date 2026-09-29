import { expect, it } from "vitest";
import type { Environment } from "@/api";
import { environmentTone, hasResources, sessionResources } from "./session-resources";

const environment = (environmentId: string, status: string, displayName?: string) =>
  ({ environmentId, status, ...(displayName ? { displayName } : {}) }) as unknown as Environment;

const config = {
  features: {
    environments: {
      selection: true,
      skills: {},
      environments: [
        { environmentId: "env-dev", access: "exec", default: true },
        { environmentId: "env-gone", access: "read" },
      ],
    },
    vfs: {
      prompts: {},
      workspaces: [
        { path: "/docs", workspaceId: "docs", access: "edit" },
        { path: "/spec", snapshotRef: "sha256:abc", access: "read" },
      ],
    },
    mcp: { servers: [{ serverId: "github", tools: ["issues"] }, { serverId: "linear" }] },
    subagents: { agents: [{ profileId: "reviewer" }], maxDepth: 2, maxConcurrent: 4 },
    web: { search: {} },
  },
};

it("names grants from the catalogs and flags what is missing or unhealthy", () => {
  const resources = sessionResources(config, "env-dev", {
    environments: [environment("env-dev", "suspended", "Dev box")],
    workspaces: [{ workspaceId: "docs", displayName: "Docs" }],
    mcpServers: [
      { serverId: "github", displayName: "GitHub", status: "active", allowedTools: ["issues", "prs", "repos"], approval: "always" },
      { serverId: "linear", status: "needsAuthConfig" },
    ],
  });
  expect(resources.environments).toMatchObject({
    selection: true, instructions: false, skills: true,
    active: { label: "Dev box", access: "exec", isDefault: true, tone: "idle", status: "suspended" },
  });
  expect(resources.environments!.items[1]).toMatchObject({ label: "env-gone", tone: "problem", active: false });
  expect(resources.files).toEqual({
    instructions: true, skills: false,
    workspaces: [
      { path: "/docs", workspaceId: "docs", label: "Docs", access: "edit", pinned: false, missing: false },
      { path: "/spec", label: "Snapshot", access: "read", pinned: true, missing: false },
    ],
  });
  expect(resources.mcp!.servers).toEqual([
    { serverId: "github", label: "GitHub", tools: ["issues"], toolTotal: 3, approval: true, tone: "ok", status: "active" },
    { serverId: "linear", label: "linear", tools: null, approval: false, tone: "problem", status: "needsAuthConfig" },
  ]);
  expect(resources.agents).toEqual({ profiles: ["reviewer"], maxDepth: 2, maxConcurrent: 4 });
  expect(resources.web).toEqual({ search: true, fetch: false });
  expect(resources.timers).toBe(false);
});

it("uses ids until catalogs load and reports nothing for an empty config", () => {
  const resources = sessionResources(config, null);
  expect(resources.environments!.active).toBeUndefined();
  expect(resources.environments!.items[0]).toMatchObject({ label: "env-dev", tone: "idle" });
  expect(resources.mcp!.servers[1]).toMatchObject({ label: "linear", tone: "ok" });
  expect(hasResources(sessionResources({ model: {} }, null))).toBe(false);
  expect(hasResources(sessionResources({ features: { timers: {} } }, null))).toBe(true);
});

it("treats sleeping machines as idle because tools wake them", () => {
  expect(["ready", "paused", "booting", "failed"].map(environmentTone)).toEqual(["ok", "idle", "busy", "problem"]);
});
