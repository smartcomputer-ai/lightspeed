import type { EnvironmentAttachment } from "@lightspeed-ai/agent-client";

export type ResourceFeature = "vfs" | "environments" | "mcp";

export function environmentAttachments(config: unknown): EnvironmentAttachment[] {
  const value = record(record(record(config).features).environments).environments;
  return Array.isArray(value) ? value.filter((item) => item && typeof item === "object") as EnvironmentAttachment[] : [];
}

export function defaultEnvironmentAttachment(config: unknown): EnvironmentAttachment | undefined {
  return environmentAttachments(config).find((attachment) => attachment.default === true);
}

export function isEnvironmentAttached(config: unknown, environmentId: string): boolean {
  return environmentAttachments(config).some((attachment) => attachment.environmentId === environmentId);
}

export function attachedEnvironments<T extends { environmentId: string }>(config: unknown, environments: T[]): T[] {
  return environments.filter((environment) => isEnvironmentAttached(config, environment.environmentId));
}

export function resourceFeatureDisableReasons(setup: unknown): Partial<Record<ResourceFeature, string>> {
  const features = record(record(record(setup).config).features);
  const result: Partial<Record<ResourceFeature, string>> = {};
  for (const [name, field, label] of [
    ["vfs", "workspaces", "workspace"],
    ["environments", "environments", "environment"],
    ["mcp", "servers", "server"],
  ] as const) {
    const attachments = record(features[name])[field];
    if (Array.isArray(attachments) && attachments.length) result[name] = `Remove the ${label} attachments before disabling this feature.`;
  }
  return result;
}

export function hasSessionFeature(config: unknown, name: ResourceFeature): boolean {
  return name in record(record(config).features);
}

export function setupResourceFeatureError(setup: unknown): string | null {
  const attachments = environmentAttachments(record(setup).config);
  const ids = new Set<string>();
  let inherited = 0;
  let defaults = 0;
  for (const attachment of attachments) {
    if (attachment.default && ++defaults > 1) return "Choose at most one default environment.";
    if (attachment.inherit) {
      if (attachment.environmentId != null || ++inherited > 1) return "Use at most one inherited environment without an environment id.";
    } else {
      const id = attachment.environmentId;
      if (!id || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(id)) return "Select an environment for each attachment.";
      if (ids.has(id)) return "Each environment may be attached only once.";
      ids.add(id);
    }
  }
  return null;
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

/// Environments a session may still select. Closed and closing environments
/// are gone for good and never offered; `provisioning`/`booting` are valid
/// selection intent (tools wait for readiness). A currently saved id is kept
/// even when its environment is closed so the editor can show it as unavailable.
export function selectableEnvironments<T extends { environmentId: string; status?: string }>(
  environments: T[],
  keepEnvironmentId?: string | null,
): T[] {
  return environments.filter((environment) =>
    environment.environmentId === keepEnvironmentId || !isTerminalEnvironmentStatus(environment.status));
}

export function isTerminalEnvironmentStatus(status: string | undefined): boolean {
  return status === "closed" || status === "closing" || status === "failed";
}

/// Selection admits nonterminal records; the runtime waits for readiness
/// when the environment is used.
export function isActivatableEnvironmentStatus(status: string | undefined): boolean {
  return status !== undefined && !isTerminalEnvironmentStatus(status);
}
