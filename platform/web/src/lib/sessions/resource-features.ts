export type ResourceFeature = "vfs" | "environments";

export function resourceFeatureDisableReasons(
  setup: unknown,
): Partial<Record<ResourceFeature, string>> {
  const document = record(setup);
  const workspaceLinkCount = arrayLength(
    record(record(record(document.config).features).vfs).workspaceLinks,
  );
  const hasEnvironmentIntent = hasProfileEnvironment(document);
  return {
    ...(workspaceLinkCount > 0
      ? { vfs: removeFirstMessage(workspaceLinkCount, "workspace link", "VFS") }
      : {}),
    ...(hasEnvironmentIntent
      ? { environments: "Clear the profile environment before disabling the Environments feature." }
      : {}),
  };
}

/// True when the profile document names an environment intent (`existing`
/// or `inherit`); absence leaves a session's selection unchanged.
export function hasProfileEnvironment(document: Record<string, unknown>): boolean {
  const environment = record(document.environment);
  return environment.type === "existing" || environment.type === "inherit";
}

export function hasSessionFeature(config: unknown, name: ResourceFeature): boolean {
  return name in record(record(config).features);
}

export function setupResourceFeatureError(setup: unknown): string | null {
  const document = record(setup);
  if (hasProfileEnvironment(document) && !hasSessionFeature(document.config, "environments")) {
    return "A profile environment requires the Environments feature to be enabled.";
  }
  const environment = record(document.environment);
  if (environment.type === "existing" && !environment.environmentId) {
    return "Select an existing environment or clear the environment mode.";
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
  return status === "closed" || status === "closing";
}

/// Statuses in which activation is admitted: ready now, or
/// provisioning/booting with tools waiting until the environment is reachable.
export function isActivatableEnvironmentStatus(status: string | undefined): boolean {
  return status === "ready" || status === "provisioning" || status === "booting";
}
