import type { MethodGroup } from "@lightspeed-ai/agent-client";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Label } from "@/components/ui/label";
import { METHOD_GROUPS, presetFor, UNIVERSE_KEY_PRESETS } from "@/lib/method-groups";

/** Preset starting points for a universe key's groups; ticking a box makes it custom. */
export function KeyPresetField({
  groups,
  onChange,
}: {
  groups: ReadonlySet<MethodGroup>;
  onChange: (next: Set<MethodGroup>) => void;
}) {
  const preset = presetFor(groups);
  return (
    <Field>
      <FieldLabel>Start from</FieldLabel>
      <div className="flex flex-wrap gap-2">
        {UNIVERSE_KEY_PRESETS.map((candidate) => (
          <Button
            key={candidate.id}
            type="button"
            size="sm"
            variant={preset?.id === candidate.id ? "secondary" : "outline"}
            aria-pressed={preset?.id === candidate.id}
            onClick={() => onChange(new Set(candidate.groups))}
          >
            {candidate.label}
          </Button>
        ))}
      </div>
      <FieldDescription>
        {preset?.description ?? "Custom: only the groups ticked below."}
      </FieldDescription>
    </Field>
  );
}

/** The method groups a new key may call, with each chosen group's caution. */
export function MethodGroupPicker({
  idPrefix,
  allowed,
  chosen,
  onChange,
}: {
  idPrefix: string;
  allowed: MethodGroup[];
  chosen: ReadonlySet<MethodGroup>;
  onChange: (next: Set<MethodGroup>) => void;
}) {
  const toggle = (group: MethodGroup, on: boolean) => {
    const next = new Set(chosen);
    if (on) next.add(group);
    else next.delete(group);
    onChange(next);
  };
  const cautions = allowed.filter((group) => chosen.has(group) && METHOD_GROUPS[group].caution);
  return (
    <fieldset className="grid gap-2">
      <legend className="mb-1 text-sm font-medium">May call</legend>
      <div className="grid gap-2 sm:grid-cols-2">
        {allowed.map((group) => (
          <div key={group} className="flex items-center gap-2">
            <Checkbox
              id={`${idPrefix}-group-${group}`}
              checked={chosen.has(group)}
              onCheckedChange={(checked) => toggle(group, checked === true)}
            />
            <Label htmlFor={`${idPrefix}-group-${group}`} className="text-sm font-normal">
              {METHOD_GROUPS[group].label}
            </Label>
          </div>
        ))}
      </div>
      {cautions.length > 0 && (
        <ul className="mt-1 grid gap-1 text-xs text-amber-700 dark:text-amber-400">
          {cautions.map((group) => (
            <li key={group}>
              <span className="font-medium">{METHOD_GROUPS[group].label}:</span> {METHOD_GROUPS[group].caution}
            </li>
          ))}
        </ul>
      )}
    </fieldset>
  );
}

/** A key's groups in a table cell: wraps to two lines at most, the full list on hover. */
export function GroupSummary({ summary }: { summary: string }) {
  return (
    <span title={summary} className="line-clamp-2 max-w-72 min-w-48 whitespace-normal">
      {summary}
    </span>
  );
}
