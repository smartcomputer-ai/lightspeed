import type { Environment } from "@/api";
import type { McpServerOption, WorkspaceOption } from "@/components/session/session-config-editor";

/// What a session can reach, read from its stored configuration and named
/// from the universe catalogs. Everything here is a grant, not usage: it says
/// what the agent may do, not what it did.

export type ResourceTone = "ok" | "idle" | "busy" | "problem";

export interface EnvironmentResource {
  key: string;
  environmentId?: string;
  label: string;
  access: string;
  active: boolean;
  isDefault: boolean;
  status?: string;
  tone: ResourceTone;
}

export interface SessionResources {
  environments?: {
    items: EnvironmentResource[];
    active?: EnvironmentResource;
    /// The agent may switch between attached environments itself.
    selection: boolean;
    instructions: boolean;
    skills: boolean;
  };
  files?: {
    workspaces: Array<{ path: string; workspaceId?: string; label: string; access: string; pinned: boolean; missing: boolean }>;
    instructions: boolean;
    skills: boolean;
  };
  mcp?: {
    servers: Array<{ serverId: string; label: string; tools: string[] | null; toolTotal?: number; approval: boolean; tone: ResourceTone; status?: string }>;
  };
  agents?: { profiles: string[]; maxDepth?: number; maxConcurrent?: number };
  web?: { search: boolean; fetch: boolean };
  timers: boolean;
}

export interface ResourceCatalogs {
  environments?: readonly Environment[];
  mcpServers?: readonly (McpServerOption & { approval?: "always" | "never" })[];
  workspaces?: readonly WorkspaceOption[];
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}
function list(value: unknown): Record<string, unknown>[] {
  return Array.isArray(value) ? value.map(record) : [];
}
function text(value: unknown): string | undefined {
  return typeof value === "string" && value ? value : undefined;
}

/// Ready is running; paused, suspended, and stopped machines wake when a tool
/// uses them, so they are idle rather than broken.
export function environmentTone(status: string | undefined): ResourceTone {
  switch (status) {
    case "ready": return "ok";
    case "paused": case "suspended": case "stopped": return "idle";
    case "provisioning": case "booting": return "busy";
    case "closing": case "closed": case "failed": return "problem";
    default: return "idle";
  }
}

export function sessionResources(
  config: unknown,
  activeEnvironmentId: string | null | undefined,
  catalogs: ResourceCatalogs = {},
): SessionResources {
  const features = record(record(config).features);
  const result: SessionResources = { timers: "timers" in features };

  if ("environments" in features) {
    const feature = record(features.environments);
    // A stored session always names its environments; a profile's `inherit`
    // attachment is resolved to the parent's id when the sub-agent starts.
    const items = list(feature.environments).flatMap((attachment): EnvironmentResource[] => {
      const environmentId = text(attachment.environmentId);
      if (!environmentId) return [];
      const known = catalogs.environments?.find((environment) => environment.environmentId === environmentId);
      return [{
        key: environmentId,
        environmentId,
        label: known?.displayName ?? environmentId,
        access: text(attachment.access) ?? "read",
        active: environmentId === activeEnvironmentId,
        isDefault: attachment.default === true,
        ...(known ? { status: known.status } : {}),
        tone: known ? environmentTone(known.status) : catalogs.environments ? "problem" : "idle",
      }];
    });
    // The active environment may come from outside the attachment list
    // (activated by the API or a profile); show it all the same.
    if (activeEnvironmentId && !items.some((item) => item.active)) {
      const known = catalogs.environments?.find((environment) => environment.environmentId === activeEnvironmentId);
      items.unshift({
        key: activeEnvironmentId, environmentId: activeEnvironmentId,
        label: known?.displayName ?? activeEnvironmentId, access: "", active: true, isDefault: false,
        ...(known ? { status: known.status } : {}), tone: known ? environmentTone(known.status) : "idle",
      });
    }
    result.environments = {
      items,
      ...(items.find((item) => item.active) ? { active: items.find((item) => item.active) } : {}),
      selection: feature.selection === true,
      instructions: "prompts" in feature,
      skills: "skills" in feature,
    };
  }

  if ("vfs" in features) {
    const feature = record(features.vfs);
    result.files = {
      workspaces: list(feature.workspaces).map((attachment) => {
        const workspaceId = text(attachment.workspaceId);
        const known = workspaceId ? catalogs.workspaces?.find((workspace) => workspace.workspaceId === workspaceId) : undefined;
        return {
          path: text(attachment.path) ?? "/",
          ...(workspaceId ? { workspaceId } : {}),
          label: known?.displayName ?? workspaceId ?? "Snapshot",
          access: text(attachment.access) ?? "read",
          pinned: Boolean(text(attachment.snapshotRef)),
          missing: Boolean(workspaceId && catalogs.workspaces && !known),
        };
      }),
      instructions: "prompts" in feature,
      skills: "skills" in feature,
    };
  }

  if ("mcp" in features) {
    const servers = list(record(features.mcp).servers).map((attachment) => {
      const serverId = text(attachment.serverId) ?? "server";
      const known = catalogs.mcpServers?.find((server) => server.serverId === serverId);
      const tools = Array.isArray(attachment.tools) ? attachment.tools.filter((tool): tool is string => typeof tool === "string") : null;
      const status = known?.status;
      return {
        serverId,
        label: known?.displayName ?? serverId,
        tools,
        ...(known?.allowedTools ? { toolTotal: known.allowedTools.length } : {}),
        approval: known?.approval === "always",
        tone: (!known && catalogs.mcpServers ? "problem" : status === "active" || !status ? "ok" : status === "unverified" ? "idle" : "problem") as ResourceTone,
        ...(status ? { status } : {}),
      };
    });
    result.mcp = { servers };
  }

  if ("subagents" in features) {
    const feature = record(features.subagents);
    result.agents = {
      profiles: list(feature.agents).flatMap((agent) => text(agent.profileId) ?? []),
      ...(typeof feature.maxDepth === "number" ? { maxDepth: feature.maxDepth } : {}),
      ...(typeof feature.maxConcurrent === "number" ? { maxConcurrent: feature.maxConcurrent } : {}),
    };
  }

  if ("web" in features) {
    const feature = record(features.web);
    result.web = { search: "search" in feature, fetch: "fetch" in feature };
  }

  return result;
}

export function hasResources(resources: SessionResources): boolean {
  return Boolean(resources.environments || resources.files || resources.mcp || resources.agents || resources.web || resources.timers);
}
