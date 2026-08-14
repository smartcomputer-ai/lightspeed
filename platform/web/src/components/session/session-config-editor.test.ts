import { describe, expect, it } from "vitest";
import {
  normalizeSessionConfig,
  workspaceLinksError,
  workspaceLinksFromConfig,
} from "./session-config-editor";

describe("workspace link config", () => {
  it("round-trips links inside the VFS feature", () => {
    const config = normalizeSessionConfig({
      features: {
        vfs: {
          tools: "edit",
          workspaceLinks: [
            {
              path: "/workspace",
              access: "readWrite",
              target: { type: "workspace", workspaceId: "primary" },
            },
            {
              path: "/skills",
              access: "readOnly",
              target: { type: "snapshot", snapshotRef: "sha256:skills" },
            },
          ],
        },
      },
    });

    expect(workspaceLinksFromConfig(config)).toEqual([
      {
        path: "/workspace",
        access: "readWrite",
        target: { type: "workspace", workspaceId: "primary" },
      },
      {
        path: "/skills",
        access: "readOnly",
        target: { type: "snapshot", snapshotRef: "sha256:skills" },
      },
    ]);
  });

  it("omits an empty workspace-link collection from the sparse config", () => {
    expect(normalizeSessionConfig({
      features: { vfs: { tools: "edit", workspaceLinks: [] } },
    })).toEqual({ features: { vfs: { tools: "edit" } } });
  });

  it("rejects overlapping paths and writable snapshots", () => {
    expect(workspaceLinksError([
      {
        path: "/workspace",
        access: "readWrite",
        target: { type: "workspace", workspaceId: "primary" },
      },
      {
        path: "/workspace/docs",
        access: "readOnly",
        target: { type: "snapshot", snapshotRef: "sha256:docs" },
      },
    ])).toContain("cannot overlap");

    expect(workspaceLinksError([{
      path: "/archive",
      access: "readWrite",
      target: { type: "snapshot", snapshotRef: "sha256:archive" },
    }])).toContain("must be read only");
  });
});

describe("environment feature config", () => {
  it("preserves independent selection and jobs grants", () => {
    expect(normalizeSessionConfig({
      features: {
        environments: {
          providers: ["sandbox-a"],
          selectionTools: true,
          jobs: true,
        },
      },
    })).toEqual({
      features: {
        environments: {
          providers: ["sandbox-a"],
          selectionTools: true,
          jobs: true,
        },
      },
    });
  });
});

describe("MCP feature config", () => {
  it("keeps only server selection and behavioral overrides", () => {
    expect(normalizeSessionConfig({
      features: {
        mcp: {
          servers: [{
            serverId: "github",
            allowedTools: ["search"],
            approval: "never",
            deferLoading: true,
          }],
        },
      },
    })).toEqual({
      features: {
        mcp: {
          servers: [{
            serverId: "github",
            allowedTools: ["search"],
            approval: "never",
            deferLoading: true,
          }],
        },
      },
    });
  });
});
