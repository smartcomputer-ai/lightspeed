import type { EnvironmentIdlePolicyView } from "@lightspeed-ai/agent-client";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";

export type IdlePolicy = EnvironmentIdlePolicyView;

export const IDLE_STAGES: Array<{ key: keyof IdlePolicy; label: string; hint: string }> = [
  { key: "pauseAfterMs", label: "Pause after", hint: "Freeze execution; RAM stays resident, resume is instant." },
  { key: "suspendAfterMs", label: "Suspend after", hint: "Save state to disk and free RAM (providers that support it)." },
  { key: "stopAfterMs", label: "Stop after", hint: "Power off; disk is kept, resume is a fresh boot." },
  { key: "closeAfterMs", label: "Close after", hint: "Destroy the environment. Not for a box a bot or several sessions rely on." },
];

/** True when every set stage is at or after the previous one (pause ≤ suspend ≤ stop ≤ close). */
export function idlePolicyIsMonotone(value: IdlePolicy | undefined): boolean {
  const ordered = IDLE_STAGES
    .map((stage) => value?.[stage.key])
    .filter((ms): ms is number => typeof ms === "number");
  return ordered.every((ms, index) => index === 0 || ms >= (ordered[index - 1] ?? 0));
}

/// Idle policy: minutes of daemon-reported idle time per stage. Empty
/// stages are omitted; stages the provider cannot realize are skipped at
/// runtime. A powered-down environment wakes when a session uses it. Shared
/// by the
/// environment create dialog, and the per-environment idle-policy editor.
export function IdlePolicyFields({
  value,
  warning,
  onChange,
}: {
  value: IdlePolicy | undefined;
  /// Shown instead of the default hint while the policy is valid.
  warning?: string;
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
  const monotone = idlePolicyIsMonotone(value);
  return (
    <Field>
      <FieldLabel>Idle policy (minutes, optional)</FieldLabel>
      <div className="grid gap-2 sm:grid-cols-2">
        {IDLE_STAGES.map((stage) => (
          <label key={stage.key} className="flex flex-col gap-1 text-xs">
            <span className="text-muted-foreground">{stage.label}</span>
            <Input
              type="number"
              min={1}
              step={1}
              value={value?.[stage.key] ? String((value[stage.key] ?? 0) / 60_000) : ""}
              placeholder="—"
              title={stage.hint}
              onChange={(event) => update(stage.key, event.target.value)}
            />
          </label>
        ))}
      </div>
      <FieldDescription
        className={
          !monotone
            ? "text-xs text-destructive"
            : warning
              ? "text-xs text-amber-700 dark:text-amber-400"
              : "text-xs"
        }
      >
        {!monotone
          ? "Stages must be non-decreasing: pause ≤ suspend ≤ stop ≤ close."
          : warning
            ?? "Measured from the environment's own idle clock; each later stage must not come before an earlier one. The environment wakes automatically when a session uses it."}
      </FieldDescription>
    </Field>
  );
}
