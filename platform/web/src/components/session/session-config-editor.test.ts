import { describe, expect, it } from "vitest";
import { createElement } from "react";
import { renderToString } from "react-dom/server";
import {
  compareModelOptions,
  modelPickerOptions,
  normalizeSessionConfig,
  SessionConfigEditor,
  type ModelOption,
  workspaceAttachmentsError,
  configError,
  mcpAttachmentError,
  workspaceAttachmentsFromConfig,
} from "./session-config-editor";

describe("specific tool choice", () => {
  it.each([
    { providerId: "openai", apiKind: "openai:responses", model: "gpt-5.5" },
    { providerId: "anthropic", apiKind: "anthropic:messages", model: "claude-opus-5" },
    { providerId: "deepseek", apiKind: "openai:completions", model: "deepseek-v4-pro" },
  ])("preserves registry IDs when saving a $apiKind configuration", (model) => {
    for (const toolId of ["env.run_process", "vfs.read_file", "custom_function"]) {
      expect(normalizeSessionConfig({
        model,
        generation: { toolChoice: { type: "specific", toolId: ` ${toolId} ` } },
      })).toEqual({
        model,
        generation: { toolChoice: { type: "specific", toolId } },
      });
    }
  });
});

describe("model picker ordering", () => {
  const option = (
    model: string,
    createdAtMs?: number,
    apiKind = "openai:responses",
  ): ModelOption => ({
    providerId: "openai",
    apiKind,
    model,
    displayName: model,
    createdAtMs,
    capabilities: {},
  });

  it("puts provider-dated models newest first and unknown dates last", () => {
    expect(
      [
        option("unknown"),
        option("older", 1_700_000_000_000),
        option("newer", 1_800_000_000_000),
      ]
        .sort(compareModelOptions)
        .map((model) => model.model),
    ).toEqual(["newer", "older", "unknown"]);
  });

  it("collapses API-kind variants while preserving an existing or pinned route", () => {
    const responses = option("gpt-5.5", 1_800_000_000_000);
    const completions = option(
      "gpt-5.5",
      1_800_000_000_000,
      "openai:completions",
    );

    expect(modelPickerOptions([completions, responses])).toEqual([responses]);
    expect(modelPickerOptions([responses, completions], completions)).toEqual([
      completions,
    ]);
    expect(
      modelPickerOptions(
        [responses, completions],
        undefined,
        "openai:completions",
      ),
    ).toEqual([completions]);
  });
});

describe("OpenAI processing tier config", () => {
  it("keeps the selector inside the collapsed model run controls", () => {
    const model = {
      providerId: "openai",
      apiKind: "openai:responses",
      model: "gpt-5.6-sol",
      displayName: "GPT-5.6",
      capabilities: {},
    } satisfies ModelOption;
    const html = renderToString(createElement(SessionConfigEditor, {
      value: { model },
      onChange: () => {},
      models: [model],
    }));

    expect(html).toContain("Model run controls");
    expect(html).not.toContain("Processing tier");
  });

  it("shows model capabilities directly below the model identifier", () => {
    const model = {
      providerId: "anthropic",
      apiKind: "anthropic:messages",
      model: "claude-opus-5",
      displayName: "Claude Opus 5",
      capabilities: {
        maxInputTokens: 1_000_000,
        maxOutputTokens: 128_000,
        parallelToolUse: true,
      },
    } satisfies ModelOption;
    const html = renderToString(createElement(SessionConfigEditor, {
      value: { model },
      onChange: () => {},
      models: [model],
    }));
    const modelId = html.indexOf("anthropic · anthropic:messages");
    const capabilities = html.indexOf("1,000,000 input tokens");
    const reasoning = html.indexOf("Reasoning effort");

    expect(modelId).toBeGreaterThanOrEqual(0);
    expect(capabilities).toBeGreaterThan(modelId);
    expect(reasoning).toBeGreaterThan(capabilities);
  });

  it("persists the tier in generation defaults for built-in OpenAI", () => {
    expect(normalizeSessionConfig({
      model: {
        providerId: "openai",
        apiKind: "openai:responses",
        model: "gpt-5.6-sol",
      },
      generation: { processingTier: "fast" },
    })).toEqual({
      model: {
        providerId: "openai",
        apiKind: "openai:responses",
        model: "gpt-5.6-sol",
      },
      generation: { processingTier: "fast" },
    });
  });

  it("drops the OpenAI-only tier when the configured provider changes", () => {
    expect(normalizeSessionConfig({
      model: {
        providerId: "deepseek",
        apiKind: "openai:completions",
        model: "deepseek-chat",
      },
      generation: { processingTier: "fast" },
    })).toEqual({
      model: {
        providerId: "deepseek",
        apiKind: "openai:completions",
        model: "deepseek-chat",
      },
    });
  });
});

describe("workspace link config", () => {
  it("round-trips links inside the VFS feature", () => {
    const config = normalizeSessionConfig({
      features: {
        vfs: {
          workspaces: [
            {
              path: "/workspace",
              access: "edit",
              workspaceId: "primary" ,
            },
            {
              path: "/skills",
              access: "read",
              snapshotRef: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" ,
            },
          ],
        },
      },
    });

    expect(workspaceAttachmentsFromConfig(config)).toEqual([
      {
        path: "/workspace",
        access: "edit",
        workspaceId: "primary" ,
      },
      {
        path: "/skills",
        access: "read",
        snapshotRef: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" ,
      },
    ]);
  });

  it("keeps an empty attachment list without granting tools", () => {
    expect(normalizeSessionConfig({
      features: { vfs: { workspaces: [] } },
    })).toEqual({ features: { vfs: { workspaces: [] } } });
  });

  it("rejects overlapping paths and writable snapshots", () => {
    expect(workspaceAttachmentsError([
      {
        path: "/workspace",
        access: "edit",
        workspaceId: "primary" ,
      },
      {
        path: "/workspace/docs",
        access: "read",
        snapshotRef: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" ,
      },
    ])).toContain("cannot overlap");

    expect(workspaceAttachmentsError([{
      path: "/archive",
      access: "edit",
      snapshotRef: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" ,
    }])).toContain("must be read only");
  });
});

describe("environment feature config", () => {
  it("preserves each attachment's access, default, and working directory", () => {
    const environments = { selection: true, environments: [
      { environmentId: "runner", access: "jobs", default: true, workingDirectory: "/project" },
      { environmentId: "logs", access: "read", workingDirectory: "/var/log" },
    ] };
    expect(normalizeSessionConfig({ features: { environments } })).toEqual({ features: { environments } });
  });

  it("keeps setup collapsed for an initially enabled capability", () => {
    const marker = "Choose the session environment here";
    const html = renderToString(createElement(SessionConfigEditor, {
      value: { features: { environments: {} } },
      onChange: () => {},
      environmentSetup: createElement("p", null, marker),
    }));

    expect(html).toContain("Environments");
    expect(html).not.toContain("Allowed providers");
    expect(html).not.toContain(marker);
    expect(html).toContain('aria-expanded="false"');
  });

  it("keeps Model run controls collapsed when optional settings already exist", () => {
    const html = renderToString(createElement(SessionConfigEditor, {
      value: { generation: { parallelToolUse: true } },
      onChange: () => {},
    }));

    expect(html).toContain("Model run controls");
    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain("Run limits");
  });

  it("renders features in task-oriented order and keeps Timers non-expandable", () => {
    const html = renderToString(createElement(SessionConfigEditor, {
      value: {
        features: {
          environments: {},
          mcp: { servers: [{ serverId: "demo" }] },
          subagents: {},
          vfs: {},
          web: { search: {} },
          timers: {},
        },
      },
      onChange: () => {},
      metadataSetup: createElement("p", null, "Metadata fields"),
      retentionSetup: createElement("p", null, "Retention fields"),
    }));
    const labels = [
      "Environments",
      "MCP Servers",
      "Sub-agents",
      "Virtual File System: Files, Instructions, Skills",
      "Web",
      "Timers",
      "Session data",
      "Session metadata",
      "Automatic deletion",
    ];
    const positions = labels.map((label) => html.indexOf(label));

    expect(positions.every((position) => position >= 0)).toBe(true);
    expect(positions).toEqual([...positions].sort((left, right) => left - right));
    const timersStart = html.indexOf('aria-label="Enable Timers"');
    const timersEnd = html.indexOf("</button></div></div>", timersStart);
    const timersHtml = html.slice(timersStart, timersEnd);
    expect(timersHtml).not.toContain("aria-expanded");
    expect(html).not.toContain("Metadata fields");
    expect(html).not.toContain("Retention fields");
  });
});

describe("sub-agent feature config", () => {
  it("preserves API-shaped profile selections across editor normalization", () => {
    const config = {
      features: {
        subagents: {
          agents: [{ profileId: "primary" }],
          maxDepth: 3,
          maxDescendants: 24,
          maxConcurrent: 6,
          deadlineMs: 7_200_000,
        },
      },
    };

    expect(normalizeSessionConfig(normalizeSessionConfig(config))).toEqual(config);
  });

  it("converts profile ids selected by the editor to the API shape", () => {
    expect(normalizeSessionConfig({
      features: {
        subagents: {
          agents: ["primary"],
        },
      },
    })).toEqual({
      features: {
        subagents: {
          agents: [{ profileId: "primary" }],
        },
      },
    });
  });
});

describe("MCP feature config", () => {
  it("keeps only server selection", () => {
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
          }],
        },
      },
    });
  });
});

describe("attachment validation and source discovery", () => {
  it("keeps source roots and per-environment directories independent", () => {
    const environments = { environments: [{ environmentId: "runner", access: "exec", workingDirectory: "/project" }], skills: { roots: ["/team"] } };
    for (const skills of [{}, { roots: [] }, { roots: ["/workspace/skills"] }]) {
      expect(normalizeSessionConfig({ features: { vfs: { skills }, environments } }))
        .toEqual({ features: { vfs: { workspaces: [], skills }, environments } });
    }
  });
  it.each(["read", "edit", "exec", "jobs"])("preserves %s access without separate tool switches", (access) => {
    const environments = { environments: [{ environmentId: "machine", access }] };
    expect(normalizeSessionConfig({ features: { environments } })).toEqual({ features: { environments } });
  });
  it("rejects duplicate defaults and ids and restricts inherit to profiles", () => {
    const attachment = { environmentId: "one", access: "read", default: true };
    expect(configError({ features: { environments: { environments: [attachment, { ...attachment, environmentId: "two" }] } } })).toContain("one default");
    expect(configError({ features: { environments: { environments: [attachment, { ...attachment, default: false }] } } })).toContain("only once");
    const config = { features: { environments: { environments: [{ inherit: true, default: true, access: "exec" }] } } };
    expect(configError(config)).toContain("sub-agent profiles");
    expect(configError(config, undefined, true)).toBeNull();
  });
  it("keeps incomplete MCP drafts while requiring a nonempty permitted subset", () => {
    for (const tools of [[], ["search"]]) {
      const config = { features: { mcp: { servers: [{ serverId: "catalog", tools }] } } };
      expect(normalizeSessionConfig(config)).toEqual(config);
      if (tools.length) expect(configError(config)).toBeNull();
      else expect(configError(config)).toContain("at least one tool");
      expect(mcpAttachmentError(config, [{ serverId: "catalog", allowedTools: ["search"] }])).toBeNull();
    }
    expect(mcpAttachmentError({ features: { mcp: { servers: [{ serverId: "catalog", tools: ["delete"] }] } } }, [{ serverId: "catalog", allowedTools: ["search"] }])).toContain("not allowed");
  });
});
