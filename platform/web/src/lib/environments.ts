import type { EnvironmentCloseReasonView, EnvironmentIdlePolicyView } from "@lightspeed-ai/sdk";
import { api, type Environment, type EnvironmentTemplate } from "@/api";

/// One page of `GET /environments`, in environment id order.
export type EnvironmentPage = {
  environments: Environment[];
  nextAfter: string | null;
};

/// The Platform route that reads one page of `environments/list`.
export function environmentPagePath(
  universeId: string,
  options: { includeClosed?: boolean; after?: string | null; limit?: number } = {},
): string {
  const query = new URLSearchParams();
  if (options.includeClosed) query.set("includeClosed", "true");
  if (options.after) query.set("after", options.after);
  if (options.limit) query.set("limit", String(options.limit));
  const suffix = query.toString();
  return `/api/v1/universes/${universeId}/environments${suffix ? `?${suffix}` : ""}`;
}

/// Every non-closed environment of the universe, read page by page. Pickers
/// and status cards use this under the `["environments", universeId]` key.
export async function listOpenEnvironments(universeId: string): Promise<Environment[]> {
  const environments: Environment[] = [];
  let after: string | null = null;
  do {
    const page: EnvironmentPage = await api<EnvironmentPage>(
      "GET",
      environmentPagePath(universeId, { after, limit: 500 }),
    );
    environments.push(...page.environments);
    after = page.nextAfter;
  } while (after);
  return environments;
}

/// The shared query for a universe's open environments.
export function openEnvironmentsQuery(universeId: string) {
  return {
    queryKey: ["environments", universeId] as const,
    queryFn: () => listOpenEnvironments(universeId),
  };
}

/// Templates of every enabled provider binding, listed live by the providers.
export function environmentTemplatesQuery(universeId: string) {
  return {
    queryKey: ["environment-templates", universeId] as const,
    queryFn: () =>
      api<EnvironmentTemplate[]>("GET", `/api/v1/universes/${universeId}/environment-templates`),
  };
}

/// Template id and provider id of a provisioned environment.
export function provisionedSource(environment: Environment) {
  return environment.source.type === "provisioned" ? environment.source : undefined;
}

/// The template a provisioned environment was created from, when listed.
export function environmentTemplate(
  environment: Environment,
  templates: readonly EnvironmentTemplate[] | undefined,
): EnvironmentTemplate | undefined {
  const source = provisionedSource(environment);
  if (!source) return undefined;
  return templates?.find((template) =>
    template.providerId === source.providerId && template.templateId === source.templateId);
}

export function environmentName(environment: Environment): string {
  return environment.displayName
    ?? provisionedSource(environment)?.templateId
    ?? environment.environmentId;
}

const CLOSE_REASONS: Record<EnvironmentCloseReasonView, string> = {
  requested: "Closed on request",
  keyRevoked: "Closed when its registration key was revoked",
  ephemeralGrace: "Closed after its daemon stayed away past the disconnect grace",
  departed: "Closed when its ephemeral daemon said goodbye",
  offlineTimeout: "Closed after its daemon stayed offline past the key's limit",
  providerEnded: "Ended by the provider",
};

export function closeReasonLabel(reason: EnvironmentCloseReasonView | null | undefined): string | null {
  return reason ? CLOSE_REASONS[reason] : null;
}

/// The lifecycle message worth showing: why it failed, or what provisioning
/// or closing waits on, or why it closed.
export function lifecycleMessage(
  environment: Pick<Environment, "state" | "stateMessage" | "closeReason">,
): string | null {
  if (environment.state === "closed") return closeReasonLabel(environment.closeReason);
  if (["failed", "provisioning", "closing"].includes(environment.state)) {
    return environment.stateMessage ?? null;
  }
  return null;
}

const IDLE_STAGES: Array<[keyof EnvironmentIdlePolicyView, string]> = [
  ["pauseAfterMs", "pause"],
  ["suspendAfterMs", "suspend"],
  ["stopAfterMs", "stop"],
  ["closeAfterMs", "close"],
];

export function describeIdlePolicy(policy: EnvironmentIdlePolicyView | null | undefined): string {
  if (!policy) return "none";
  const stages = IDLE_STAGES
    .filter(([key]) => typeof policy[key] === "number")
    .map(([key, label]) => `${label} after ${formatDuration(policy[key] as number)}`);
  return stages.length ? stages.join(", ") : "none";
}

export function formatDuration(ms: number): string {
  if (ms % 3_600_000 === 0) return `${ms / 3_600_000}h`;
  if (ms % 60_000 === 0) return `${ms / 60_000}m`;
  if (ms % 1_000 === 0) return `${ms / 1_000}s`;
  return `${ms}ms`;
}
