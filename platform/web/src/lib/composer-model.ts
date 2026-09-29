import { reasoningEffortTiers, type MessageRunOptions } from "@lightspeed/platform-shared";
import type { ModelConfig, ModelOption } from "@/api";
import { modelFromConfig } from "@/lib/model-defaults";

/// What the composer's model pill has chosen for the messages it sends.
/// Each field is an override of the session configuration; an absent field
/// follows the session. Other generation settings, such as the processing
/// tier, are edited in the session configuration only.
export interface RunChoice {
  model?: string;
  reasoningEffort?: string;
}

export interface ComposerModelChoice {
  /// The session's pinned route; only the model name may change per run.
  route: ModelConfig;
  model: string;
  modelLabel: string;
  reasoningEffort?: string;
  session: { model: string; reasoningEffort?: string };
  /// Models on the session's provider and API, the session model first.
  models: Array<{ model: string; displayName: string }>;
  efforts: readonly string[];
  /// The overrides to send with a message; undefined when the choice
  /// matches the session configuration.
  options?: MessageRunOptions;
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" ? value as Record<string, unknown> : {};
}

/// Resolves the pill's state from the session config, discovered models, and
/// the stored choice. Returns null when the session has no model route.
export function composerModelChoice(
  config: unknown,
  discovered: readonly ModelOption[] | undefined,
  choice: RunChoice,
): ComposerModelChoice | null {
  const route = modelFromConfig(config);
  if (!route) return null;
  const generation = record(record(config).generation);
  const session = {
    model: route.model,
    ...(typeof generation.reasoningEffort === "string" && generation.reasoningEffort ? { reasoningEffort: generation.reasoningEffort } : {}),
  };
  const sameRoute = (discovered ?? []).filter((option) => option.providerId === route.providerId && option.apiKind === route.apiKind);
  const model = choice.model ?? session.model;
  const models = new Map<string, string>();
  models.set(session.model, sameRoute.find((option) => option.model === session.model)?.displayName ?? session.model);
  for (const option of sameRoute) if (!models.has(option.model)) models.set(option.model, option.displayName || option.model);
  if (!models.has(model)) models.set(model, model);
  const discoveredEfforts = sameRoute.find((option) => option.model === model)?.capabilities.reasoningEfforts;
  const reasoningEffort = choice.reasoningEffort ?? session.reasoningEffort;
  const options: MessageRunOptions = {
    ...(model !== session.model ? { model: { ...route, model } } : {}),
    ...(reasoningEffort && reasoningEffort !== session.reasoningEffort ? { reasoningEffort } : {}),
  };
  return {
    route,
    model,
    modelLabel: models.get(model) ?? model,
    ...(reasoningEffort ? { reasoningEffort } : {}),
    session,
    models: [...models].map(([name, displayName]) => ({ model: name, displayName })),
    efforts: discoveredEfforts?.length ? discoveredEfforts : reasoningEffortTiers(route.apiKind),
    ...(Object.keys(options).length ? { options } : {}),
  };
}

/// Drops fields that equal the session's value, so a choice that returns to
/// the session setting stops overriding it.
export function normalizeRunChoice(choice: RunChoice, session: ComposerModelChoice["session"]): RunChoice {
  return {
    ...(choice.model && choice.model !== session.model ? { model: choice.model } : {}),
    ...(choice.reasoningEffort && choice.reasoningEffort !== session.reasoningEffort ? { reasoningEffort: choice.reasoningEffort } : {}),
  };
}

const EFFORT_LABELS: Readonly<Record<string, string>> = {
  none: "None", minimal: "Minimal", low: "Low", medium: "Medium", high: "High", xhigh: "Extra high", max: "Max",
};
export function effortLabel(effort: string | undefined): string {
  if (!effort) return "Default";
  return EFFORT_LABELS[effort] ?? effort.charAt(0).toUpperCase() + effort.slice(1);
}
/// The pill's compact effort word.
export function effortShortLabel(effort: string | undefined): string {
  return effort === "xhigh" ? "XHigh" : effortLabel(effort);
}

export function readRunChoice(key: string): RunChoice {
  try {
    const value = record(JSON.parse(window.localStorage.getItem(key) ?? "{}"));
    return {
      ...(typeof value.model === "string" && value.model ? { model: value.model } : {}),
      ...(typeof value.reasoningEffort === "string" && value.reasoningEffort ? { reasoningEffort: value.reasoningEffort } : {}),
    };
  } catch {
    return {};
  }
}

export function writeRunChoice(key: string, choice: RunChoice): void {
  try {
    if (Object.keys(choice).length) window.localStorage.setItem(key, JSON.stringify(choice));
    else window.localStorage.removeItem(key);
  } catch {
    // The choice still applies in this view when storage is blocked.
  }
}

/// The session config after saving the choice as its default: the model
/// name and reasoning effort change, everything else stays as stored.
export function configWithRunChoice(config: Record<string, unknown>, choice: ComposerModelChoice): Record<string, unknown> {
  const generation = { ...record(config.generation) };
  if (choice.reasoningEffort) generation.reasoningEffort = choice.reasoningEffort;
  return {
    ...config,
    model: { ...choice.route, model: choice.model },
    ...(Object.keys(generation).length ? { generation } : {}),
  };
}
