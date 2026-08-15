export type ResourceFeature = "vfs" | "environments";

export function resourceFeatureDisableReasons(
  setup: unknown,
): Partial<Record<ResourceFeature, string>> {
  const document = record(setup);
  const workspaceLinkCount = arrayLength(
    record(record(record(document.config).features).vfs).workspaceLinks,
  );
  const hasActiveEnvironment = typeof document.activeEnvironmentId === "string"
    && document.activeEnvironmentId.length > 0;
  return {
    ...(workspaceLinkCount > 0
      ? { vfs: removeFirstMessage(workspaceLinkCount, "workspace link", "VFS") }
      : {}),
    ...(hasActiveEnvironment
      ? { environments: "Clear the active environment before disabling the Environments feature." }
      : {}),
  };
}

export function hasSessionFeature(config: unknown, name: ResourceFeature): boolean {
  return name in record(record(config).features);
}

export function setupResourceFeatureError(setup: unknown): string | null {
  const document = record(setup);
  if (typeof document.activeEnvironmentId === "string"
    && document.activeEnvironmentId.length > 0
    && !hasSessionFeature(document.config, "environments")) {
    return "An active environment requires the Environments feature to be enabled.";
  }
  return null;
}

function arrayLength(value: unknown): number {
  return Array.isArray(value) ? value.length : 0;
}

function removeFirstMessage(count: number, resource: string, feature: string): string {
  const resources = count === 1 ? `the ${resource}` : `all ${count} ${resource}s`;
  return `Remove ${resources} before disabling ${feature}.`;
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
}
