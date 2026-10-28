import type { EnvironmentIdlePolicyView, EnvironmentPowerStateView } from "@lightspeed-ai/sdk";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";

type IdlePolicy = EnvironmentIdlePolicyView;

/// Each stage with the power state its provider template must support;
/// closing needs none.
const IDLE_STAGES: Array<{
  key: keyof IdlePolicy;
  label: string;
  hint: string;
  power?: EnvironmentPowerStateView;
}> = [
  { key: "pauseAfterMs", label: "Pause after", hint: "Freeze execution; memory stays resident and resume is instant.", power: "paused" },
  { key: "suspendAfterMs", label: "Suspend after", hint: "Save state to disk and free memory.", power: "suspended" },
  { key: "stopAfterMs", label: "Stop after", hint: "Power off; the disk is kept and resume is a fresh boot.", power: "stopped" },
  { key: "closeAfterMs", label: "Close after", hint: "Destroy the environment." },
];

/// The stages a template can carry out, in escalation order.
export function idleStagesFor(powerStates: EnvironmentPowerStateView[]) {
  return IDLE_STAGES.filter((stage) => !stage.power || powerStates.includes(stage.power));
}

/// Why `policy` cannot be sent, or `null` when it is valid: at least one
/// stage, and each set stage no earlier than the one before it.
export function idlePolicyProblem(policy: IdlePolicy | undefined): string | null {
  const thresholds = IDLE_STAGES
    .map((stage) => policy?.[stage.key])
    .filter((ms): ms is number => typeof ms === "number");
  if (thresholds.length === 0) return "Set at least one stage.";
  if (!thresholds.every((ms, index) => index === 0 || ms >= (thresholds[index - 1] ?? 0))) {
    return "Stages must not decrease: pause ≤ suspend ≤ stop ≤ close.";
  }
  return null;
}

/// Minutes of envd-reported idle time per stage, for the stages the template
/// supports. Empty stages are omitted from the policy.
export function IdlePolicyFields({
  value,
  powerStates,
  onChange,
}: {
  value: IdlePolicy | undefined;
  powerStates: EnvironmentPowerStateView[];
  onChange: (policy: IdlePolicy | undefined) => void;
}) {
  const update = (key: keyof IdlePolicy, minutes: string) => {
    const next: IdlePolicy = { ...(value ?? {}) };
    const parsed = Number(minutes);
    if (!minutes.trim() || !Number.isFinite(parsed) || parsed <= 0) {
      delete next[key];
    } else {
      next[key] = Math.round(parsed * 60_000);
    }
    onChange(Object.keys(next).length ? next : undefined);
  };
  const problem = idlePolicyProblem(value);
  return (
    <Field>
      <FieldLabel>Idle policy (minutes)</FieldLabel>
      <div className="grid gap-2 sm:grid-cols-2">
        {idleStagesFor(powerStates).map((stage) => (
          <label key={stage.key} className="flex flex-col gap-1 text-xs">
            <span className="text-muted-foreground">{stage.label}</span>
            <Input
              type="number"
              min={1}
              step={1}
              aria-label={stage.label}
              value={value?.[stage.key] ? String((value[stage.key] ?? 0) / 60_000) : ""}
              placeholder="—"
              title={stage.hint}
              onChange={(event) => update(stage.key, event.target.value)}
            />
          </label>
        ))}
      </div>
      <FieldDescription className={problem ? "text-xs text-destructive" : "text-xs"}>
        {problem ??
          "Measured by the environment's own idle clock. The provider applies it; it cannot be changed after the environment is created."}
      </FieldDescription>
    </Field>
  );
}
