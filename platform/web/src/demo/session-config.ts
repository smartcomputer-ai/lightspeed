import { defaultEnvironmentAttachment, environmentAttachments, isEnvironmentAttached } from "@/components/profiles/resource-features";
import type { UniverseState } from "./store";
import type { Environment, ModelConfig } from "@/api";
import { modelOf } from "./engine";

/// The session's own copy of a profile config, with the model the demo
/// answers as when the profile leaves it open.
export function sessionConfig(config: Record<string, unknown>, fallback?: ModelConfig | null): Record<string, unknown> | null {
  const copy = structuredClone(config);
  const model = modelOf(copy) ?? fallback;
  return model ? { ...copy, model: { ...model } } : null;
}

/// Provisioning, powered-down and disconnected environments are valid
/// activation targets; a terminal or terminating one is not.
export function usableEnvironment(environment: Environment): boolean {
  return (
    environment.state !== "closed" &&
    environment.state !== "closing" &&
    environment.state !== "failed"
  );
}

/// Profiles select resources whose lifecycle is managed independently.
export function resolveSessionEnvironment(
  universe: UniverseState,
  config: Record<string, unknown>,
): { environmentId: string | null } | { error: string } {
  if (environmentAttachments(config).some((attachment) => attachment.inherit)) return { error: "inherited environments require a parent session" };
  const environmentId = defaultEnvironmentAttachment(config)?.environmentId;
  if (!environmentId) return { environmentId: null };
  if (!isEnvironmentAttached(config, environmentId)) return { error: `environment is not attached: ${environmentId}` };
  const environment = universe.environments.get(environmentId);
  if (!environment) return { error: `environment not found: ${environmentId}` };
  if (!usableEnvironment(environment)) return { error: `environment is ${environment.status}: ${environmentId}` };
  return { environmentId };
}
