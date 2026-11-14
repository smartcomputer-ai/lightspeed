import { useState } from "react";
import { ConfigureIcon } from "@/icons";
import { Button } from "@/components/ui/button";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

export type CronFrequency = "minutes" | "hourly" | "daily" | "weekdays" | "weekly" | "monthly";

export type CronBuilderState = {
  frequency: CronFrequency;
  interval: number;
  minute: number;
  time: string;
  weekday: number;
  monthday: number;
};

const DEFAULT_BUILDER_STATE: CronBuilderState = {
  frequency: "daily",
  interval: 15,
  minute: 0,
  time: "09:00",
  weekday: 1,
  monthday: 1,
};

const WEEKDAYS = [
  [0, "Sunday"],
  [1, "Monday"],
  [2, "Tuesday"],
  [3, "Wednesday"],
  [4, "Thursday"],
  [5, "Friday"],
  [6, "Saturday"],
] as const;

function bounded(value: number, minimum: number, maximum: number): number {
  return Math.min(Math.max(Math.round(Number.isFinite(value) ? value : minimum), minimum), maximum);
}

function timeParts(value: string): [number, number] {
  const match = /^(\d{2}):(\d{2})$/.exec(value);
  return match
    ? [bounded(Number(match[1]), 0, 23), bounded(Number(match[2]), 0, 59)]
    : [9, 0];
}

export function cronFromBuilder(state: CronBuilderState): string {
  const [hour, minute] = timeParts(state.time);
  switch (state.frequency) {
    case "minutes":
      return `*/${bounded(state.interval, 1, 59)} * * * *`;
    case "hourly":
      return `${bounded(state.minute, 0, 59)} * * * *`;
    case "daily":
      return `${minute} ${hour} * * *`;
    case "weekdays":
      return `${minute} ${hour} * * 1-5`;
    case "weekly":
      return `${minute} ${hour} * * ${bounded(state.weekday, 0, 6)}`;
    case "monthly":
      return `${minute} ${hour} ${bounded(state.monthday, 1, 31)} * *`;
  }
}

export function cronBuilderFromExpression(value: string): CronBuilderState {
  const cron = value.trim();
  const macro = {
    "@hourly": { frequency: "hourly" as const },
    "@daily": { frequency: "daily" as const },
    "@weekly": { frequency: "weekly" as const },
    "@monthly": { frequency: "monthly" as const },
  }[cron];
  if (macro) return { ...DEFAULT_BUILDER_STATE, ...macro };

  const fields = cron.split(/\s+/);
  if (fields.length !== 5) return { ...DEFAULT_BUILDER_STATE };
  const [minute, hour, monthday, month, weekday] = fields;
  const interval = /^\*\/(\d+)$/.exec(minute ?? "");
  if (interval && hour === "*" && monthday === "*" && month === "*" && weekday === "*") {
    return {
      ...DEFAULT_BUILDER_STATE,
      frequency: "minutes",
      interval: bounded(Number(interval[1]), 1, 59),
    };
  }

  const minuteNumber = Number(minute);
  if (!Number.isInteger(minuteNumber) || minuteNumber < 0 || minuteNumber > 59) {
    return { ...DEFAULT_BUILDER_STATE };
  }
  if (hour === "*" && monthday === "*" && month === "*" && weekday === "*") {
    return { ...DEFAULT_BUILDER_STATE, frequency: "hourly", minute: minuteNumber };
  }

  const hourNumber = Number(hour);
  if (!Number.isInteger(hourNumber) || hourNumber < 0 || hourNumber > 23 || month !== "*") {
    return { ...DEFAULT_BUILDER_STATE };
  }
  const time = `${String(hourNumber).padStart(2, "0")}:${String(minuteNumber).padStart(2, "0")}`;
  if (monthday === "*" && weekday === "1-5") {
    return { ...DEFAULT_BUILDER_STATE, frequency: "weekdays", time };
  }
  if (monthday === "*" && weekday === "*") {
    return { ...DEFAULT_BUILDER_STATE, frequency: "daily", time };
  }
  if (monthday === "*" && /^[0-6]$/.test(weekday ?? "")) {
    return { ...DEFAULT_BUILDER_STATE, frequency: "weekly", time, weekday: Number(weekday) };
  }
  if (/^\d{1,2}$/.test(monthday ?? "") && weekday === "*") {
    return {
      ...DEFAULT_BUILDER_STATE,
      frequency: "monthly",
      time,
      monthday: bounded(Number(monthday), 1, 31),
    };
  }
  return { ...DEFAULT_BUILDER_STATE };
}

function builderSummary(state: CronBuilderState): string {
  switch (state.frequency) {
    case "minutes":
      return `Every ${bounded(state.interval, 1, 59)} minutes`;
    case "hourly":
      return `Every hour at minute ${bounded(state.minute, 0, 59)}`;
    case "daily":
      return `Every day at ${state.time}`;
    case "weekdays":
      return `Every weekday at ${state.time}`;
    case "weekly":
      return `Every ${WEEKDAYS.find(([value]) => value === state.weekday)?.[1] ?? "week"} at ${state.time}`;
    case "monthly":
      return `Day ${bounded(state.monthday, 1, 31)} of every month at ${state.time}`;
  }
}

export function CronBuilder({ value, onChange }: { value: string; onChange: (value: string) => void }) {
  const [open, setOpen] = useState(false);
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger
        render={<Button type="button" variant="outline" size="xs" />}
      >
        <ConfigureIcon className="size-3.5" />
        Build visually
      </PopoverTrigger>
      <PopoverContent align="start" sideOffset={8}>
        {open && (
          <CronBuilderPanel
            initial={cronBuilderFromExpression(value)}
            onApply={(cron) => {
              onChange(cron);
              setOpen(false);
            }}
          />
        )}
      </PopoverContent>
    </Popover>
  );
}

function CronBuilderPanel({
  initial,
  onApply,
}: {
  initial: CronBuilderState;
  onApply: (cron: string) => void;
}) {
  const [draft, setDraft] = useState(initial);
  const expression = cronFromBuilder(draft);
  return (
    <div className="grid gap-4 p-4">
      <div className="grid gap-0.5">
        <h3 className="text-sm font-semibold">Visual schedule</h3>
        <p className="text-xs text-muted-foreground">Choose a common pattern, then apply it to the cron field.</p>
      </div>
      <Field>
        <FieldLabel>Repeat</FieldLabel>
        <Select
          value={draft.frequency}
          onValueChange={(value) => value && setDraft({ ...draft, frequency: value as CronFrequency })}
        >
          <SelectTrigger><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="minutes">Every few minutes</SelectItem>
            <SelectItem value="hourly">Every hour</SelectItem>
            <SelectItem value="daily">Every day</SelectItem>
            <SelectItem value="weekdays">Weekdays</SelectItem>
            <SelectItem value="weekly">Every week</SelectItem>
            <SelectItem value="monthly">Every month</SelectItem>
          </SelectContent>
        </Select>
      </Field>

      {draft.frequency === "minutes" && (
        <Field>
          <FieldLabel htmlFor="cron-builder-interval">Interval in minutes</FieldLabel>
          <Input
            id="cron-builder-interval"
            type="number"
            min={1}
            max={59}
            value={draft.interval}
            onChange={(event) => setDraft({ ...draft, interval: Number(event.target.value) })}
          />
        </Field>
      )}
      {draft.frequency === "hourly" && (
        <Field>
          <FieldLabel htmlFor="cron-builder-minute">Minute past the hour</FieldLabel>
          <Input
            id="cron-builder-minute"
            type="number"
            min={0}
            max={59}
            value={draft.minute}
            onChange={(event) => setDraft({ ...draft, minute: Number(event.target.value) })}
          />
        </Field>
      )}
      {(draft.frequency === "daily" || draft.frequency === "weekdays") && (
        <TimeInput draft={draft} setDraft={setDraft} />
      )}
      {draft.frequency === "weekly" && (
        <div className="grid gap-3 sm:grid-cols-2">
          <Field>
            <FieldLabel>Day</FieldLabel>
            <Select
              value={String(draft.weekday)}
              onValueChange={(value) => value !== null && setDraft({ ...draft, weekday: Number(value) })}
            >
              <SelectTrigger><SelectValue /></SelectTrigger>
              <SelectContent>
                {WEEKDAYS.map(([value, label]) => <SelectItem key={value} value={String(value)}>{label}</SelectItem>)}
              </SelectContent>
            </Select>
          </Field>
          <TimeInput draft={draft} setDraft={setDraft} />
        </div>
      )}
      {draft.frequency === "monthly" && (
        <div className="grid gap-3 sm:grid-cols-2">
          <Field>
            <FieldLabel htmlFor="cron-builder-monthday">Day of month</FieldLabel>
            <Input
              id="cron-builder-monthday"
              type="number"
              min={1}
              max={31}
              value={draft.monthday}
              onChange={(event) => setDraft({ ...draft, monthday: Number(event.target.value) })}
            />
          </Field>
          <TimeInput draft={draft} setDraft={setDraft} />
        </div>
      )}

      <div className="flex min-w-0 items-center gap-3 rounded-md bg-muted px-3 py-2">
        <span className="min-w-0 flex-1 text-xs text-muted-foreground">
          <span className="block">{builderSummary(draft)}</span>
          <code className="block truncate" title={expression}>{expression}</code>
        </span>
        <Button type="button" size="sm" onClick={() => onApply(expression)}>Use schedule</Button>
      </div>
    </div>
  );
}

function TimeInput({
  draft,
  setDraft,
}: {
  draft: CronBuilderState;
  setDraft: (draft: CronBuilderState) => void;
}) {
  return (
    <Field>
      <FieldLabel htmlFor="cron-builder-time">Time</FieldLabel>
      <Input
        id="cron-builder-time"
        type="time"
        value={draft.time}
        onChange={(event) => setDraft({ ...draft, time: event.target.value })}
      />
    </Field>
  );
}
