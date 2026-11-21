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

export function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}
