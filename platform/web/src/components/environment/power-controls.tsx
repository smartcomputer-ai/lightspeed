import { useMutation, useQueryClient } from "@tanstack/react-query";
import type { EnvironmentPowerStateView } from "@lightspeed-ai/sdk";
import { api, type Environment } from "@/api";
import { Button } from "@/components/ui/button";

const OPTIONS: Array<[EnvironmentPowerStateView, string]> = [
  ["running", "Resume"],
  ["paused", "Pause"],
  ["suspended", "Suspend"],
  ["stopped", "Stop"],
];

/// The power state the provider last reported, while the target is active.
function observedPower(environment: Environment): EnvironmentPowerStateView | null {
  if (environment.source.type !== "provisioned") return null;
  const observed = environment.source.observed;
  if (!observed) return "running";
  return observed.phase === "active" ? observed.power ?? "running" : null;
}

/// One-shot `environments/power/put` buttons for an open provisioned
/// environment. The provider moves the target at once and owns every later
/// transition; a powered-down environment also wakes when a tool uses it.
/// `powerStates` are the states the environment's template supports.
/// `onChanged` lets a caller refresh its own query on top of the
/// environments list.
export function EnvironmentPowerControls({
  universeId,
  environment,
  powerStates,
  onChanged,
  size = "xs",
}: {
  universeId: string;
  environment: Environment;
  powerStates: readonly EnvironmentPowerStateView[];
  onChanged?: () => void;
  size?: "xs" | "sm";
}) {
  const queryClient = useQueryClient();
  const power = useMutation({
    mutationFn: (next: EnvironmentPowerStateView) => api<Environment>(
      "PUT",
      `/api/v1/universes/${universeId}/environments/${encodeURIComponent(environment.environmentId)}/power`,
      { power: next },
    ),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["environments", universeId] });
      onChanged?.();
    },
  });
  if (environment.source.type !== "provisioned" || environment.state !== "open" || powerStates.length === 0) {
    return null;
  }
  const observed = observedPower(environment);
  return (
    <>
      {OPTIONS
        .filter(([state]) => powerStates.includes(state) && state !== observed)
        .map(([state, label]) => (
          <Button
            key={state}
            variant="outline"
            size={size}
            disabled={power.isPending}
            onClick={() => power.mutate(state)}
          >
            {label}
          </Button>
        ))}
      {power.error && <span className="text-xs text-destructive">{power.error.message}</span>}
    </>
  );
}
