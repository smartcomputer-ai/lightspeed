import { useMutation, useQueryClient } from "@tanstack/react-query";
import { api, type Environment } from "@/api";
import { Button } from "@/components/ui/button";

/// Observed steady power state derived from the lifecycle status.
export function observedPower(environment: Environment): Environment["desiredPower"] | null {
  switch (environment.status) {
    case "ready":
      return "running";
    case "paused":
      return "paused";
    case "suspended":
      return "suspended";
    case "offline":
      return "stopped";
    default:
      return null;
  }
}

export function powerDiverges(environment: Environment): boolean {
  if (environment.source.type !== "provisioned") return false;
  const observed = observedPower(environment);
  return observed !== null && observed !== environment.desiredPower;
}

const IDLE_STAGES: Array<[keyof NonNullable<Environment["idlePolicy"]>, string]> = [
  ["pauseAfterMs", "pause"],
  ["suspendAfterMs", "suspend"],
  ["stopAfterMs", "stop"],
  ["closeAfterMs", "close"],
];

export function describeIdlePolicy(policy: Environment["idlePolicy"] | undefined): string {
  if (!policy) return "none";
  const stages = IDLE_STAGES
    .filter(([key]) => policy[key] !== undefined && policy[key] !== null)
    .map(([key, label]) => `${label} after ${formatDuration(policy[key] as number)}`);
  return stages.length ? stages.join(", ") : "none";
}

export function formatDuration(ms: number): string {
  if (ms % 3_600_000 === 0) return `${ms / 3_600_000}h`;
  if (ms % 60_000 === 0) return `${ms / 60_000}m`;
  if (ms % 1_000 === 0) return `${ms / 1_000}s`;
  return `${ms}ms`;
}

/// Power intent: pause/suspend/stop/resume a provisioned environment.
/// Only the states the provider reported are offered; a powered-down
/// environment also wakes by itself when a session uses it. `onChanged`
/// lets a caller refresh its own query on top of the environments list.
export function EnvironmentPowerControls({
  universeId,
  environment,
  onChanged,
  size = "xs",
}: {
  universeId: string;
  environment: Environment;
  onChanged?: () => void;
  size?: "xs" | "sm";
}) {
  const queryClient = useQueryClient();
  const power = useMutation({
    mutationFn: (next: Environment["desiredPower"]) => api<Environment>(
      "PUT",
      `/api/v1/universes/${universeId}/environments/${encodeURIComponent(environment.environmentId)}/power`,
      { power: next },
    ),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["environments", universeId] });
      onChanged?.();
    },
  });
  const supported = environment.incarnation.powerStates ?? [];
  const closed = ["closing", "closed", "failed"].includes(environment.status);
  if (closed || supported.length === 0) return null;
  const busy = power.isPending || powerDiverges(environment);
  const observed = observedPower(environment);
  const options: Array<[Environment["desiredPower"], string]> = [
    ["running", "Resume"],
    ["paused", "Pause"],
    ["suspended", "Suspend"],
    ["stopped", "Stop"],
  ];
  return (
    <>
      {options
        .filter(([state]) => supported.includes(state) && state !== environment.desiredPower)
        .filter(([state]) => (state === "running" ? observed !== "running" : observed === "running" || observed === null))
        .map(([state, label]) => (
          <Button
            key={state}
            variant="outline"
            size={size}
            disabled={busy}
            onClick={() => power.mutate(state)}
          >
            {label}
          </Button>
        ))}
      {busy && !power.isPending && (
        <span className="text-xs text-muted-foreground">Converging to {environment.desiredPower}…</span>
      )}
      {power.error && <span className="text-xs text-destructive">{power.error.message}</span>}
    </>
  );
}
