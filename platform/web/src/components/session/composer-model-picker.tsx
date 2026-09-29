import { useState } from "react";
import { Box, Check, ChevronDown, Search } from "lucide-react";
import {
  effortLabel,
  effortShortLabel,
  type ComposerModelChoice,
  type RunChoice,
} from "@/lib/composer-model";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { cn } from "@/lib/utils";

/// The model pill: the model and reasoning effort the next message runs
/// with. Choices override the session for each message sent from here and
/// never change the stored configuration unless saved as its default.
export function ModelPicker({ choice, stored, runActive, canSaveDefault, onChange, onSaveDefault }: {
  choice: ComposerModelChoice;
  stored: RunChoice;
  runActive: boolean;
  canSaveDefault: boolean;
  onChange: (next: RunChoice) => void;
  onSaveDefault: () => Promise<void>;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string>();
  const overridden = choice.options !== undefined;
  const search = query.trim().toLowerCase();
  const models = choice.models.filter((option) =>
    !search || option.model.toLowerCase().includes(search) || option.displayName.toLowerCase().includes(search));
  const exact = choice.models.some((option) => option.model === query.trim());
  const effortOptions = [
    ...(choice.session.reasoningEffort ? [] : [undefined]),
    ...choice.efforts,
  ];
  const pick = (patch: RunChoice) => {
    setSaveError(undefined);
    onChange({ ...stored, ...patch });
  };
  const save = async () => {
    setSaving(true);
    setSaveError(undefined);
    try {
      await onSaveDefault();
    } catch (cause) {
      setSaveError(cause instanceof Error ? cause.message : "Could not save the session default.");
    } finally {
      setSaving(false);
    }
  };
  const summary = `${choice.modelLabel}, ${effortLabel(choice.reasoningEffort).toLowerCase()} effort`;

  return (
    <Popover open={open} onOpenChange={(next) => { setOpen(next); if (!next) setQuery(""); }}>
      <PopoverTrigger render={
        <Button type="button" variant="ghost" size="sm"
          aria-label={`Model: ${summary}${overridden ? " (differs from the session)" : ""}. Change model or effort`}
          className="relative h-8 max-w-full min-w-0 shrink gap-1.5 px-2 text-xs font-medium" />
      }>
        <Box className="hidden size-3.5 text-muted-foreground sm:block" aria-hidden />
        <span className="min-w-0 max-w-28 truncate sm:max-w-48">{choice.modelLabel}</span>
        <span className="hidden text-muted-foreground/50 sm:inline" aria-hidden>·</span>
        <span className="hidden text-muted-foreground sm:inline">{effortShortLabel(choice.reasoningEffort)}</span>
        <ChevronDown className="size-3 text-muted-foreground" aria-hidden />
        {overridden && <span className="absolute top-1 right-1 size-1.5 rounded-full bg-primary" aria-hidden />}
      </PopoverTrigger>
      <PopoverContent side="top" align="start" sideOffset={8} className="w-[min(34rem,calc(100vw-2rem))] overflow-hidden p-0">
        <div className="flex max-h-[min(22rem,calc(var(--available-height)-4rem))] min-h-0 flex-col sm:flex-row">
          <section aria-label="Model" className="flex min-h-0 min-w-0 flex-1 flex-col border-b sm:border-r sm:border-b-0">
            <label className="m-2 mb-1 flex h-8 items-center gap-2 rounded-md bg-muted px-2 text-muted-foreground focus-within:ring-2 focus-within:ring-ring/50">
              <Search className="size-3.5 shrink-0" aria-hidden />
              <input autoFocus value={query} onChange={(event) => setQuery(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" && query.trim()) {
                    event.preventDefault();
                    pick({ model: (models[0]?.model && !exact && models.length === 1 ? models[0].model : query.trim()) });
                    setQuery("");
                  }
                }}
                placeholder={`Search ${choice.route.providerId} models`} aria-label="Search models"
                className="min-w-0 flex-1 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground" />
            </label>
            <div role="radiogroup" aria-label="Model" className="min-h-0 flex-1 overflow-y-auto px-1.5 pb-1.5">
              {models.map((option) => (
                <Option key={option.model} checked={option.model === choice.model} onSelect={() => pick({ model: option.model })}>
                  <span className="flex min-w-0 flex-1 flex-col leading-tight">
                    <span className="truncate">{option.displayName}</span>
                    {option.displayName !== option.model && <span className="truncate text-[11px] text-muted-foreground">{option.model}</span>}
                  </span>
                  {option.model === choice.session.model && (
                    <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">Session</span>
                  )}
                </Option>
              ))}
              {query.trim() && !exact && (
                <Option checked={false} onSelect={() => { pick({ model: query.trim() }); setQuery(""); }}>
                  <span className="truncate">Use “{query.trim()}”</span>
                </Option>
              )}
              {!models.length && !query.trim() && <p className="px-2 py-3 text-xs text-muted-foreground">No models were discovered.</p>}
            </div>
          </section>
          <section aria-label="Reasoning effort" className="flex min-h-0 flex-col overflow-y-auto p-1.5 sm:w-48">
            <h3 className="px-2 pt-1.5 pb-1 text-[11px] font-medium tracking-wide text-muted-foreground uppercase">Reasoning effort</h3>
            <div role="radiogroup" aria-label="Reasoning effort">
              {effortOptions.map((effort) => (
                <Option key={effort ?? "default"} checked={effort === choice.reasoningEffort}
                  onSelect={() => pick({ reasoningEffort: effort })}>
                  <span className="flex-1">{effort ? effortLabel(effort) : "Provider default"}</span>
                  {effort && effort === choice.session.reasoningEffort && (
                    <span className="rounded bg-muted px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">Session</span>
                  )}
                </Option>
              ))}
            </div>
          </section>
        </div>
        <footer className="flex flex-wrap items-center gap-2 border-t px-3 py-2 text-xs text-muted-foreground">
          <p className="min-w-0 flex-1" role={saveError ? "alert" : undefined}>
            {saveError
              ? <span className="text-destructive">{saveError}</span>
              : overridden
                ? "Applies to each message you send from here. Steering joins the running run as it is."
                : "Matches the session configuration."}
          </p>
          {overridden && (
            <Button type="button" variant="ghost" size="xs" onClick={() => { setSaveError(undefined); onChange({}); }}>Reset</Button>
          )}
          {canSaveDefault && (
            <Button type="button" variant="outline" size="xs" disabled={!overridden || runActive || saving}
              title={runActive ? "The session configuration can change once no run is active." : undefined}
              onClick={() => void save()}>
              {saving ? "Saving…" : "Save as session default"}
            </Button>
          )}
        </footer>
      </PopoverContent>
    </Popover>
  );
}

function Option({ checked, onSelect, children }: { checked: boolean; onSelect: () => void; children: React.ReactNode }) {
  return (
    <button type="button" role="radio" aria-checked={checked} onClick={onSelect}
      className={cn(
        "flex min-h-8 w-full items-center gap-2 rounded-md px-2 py-1 text-left text-sm outline-none transition-colors hover:bg-muted focus-visible:bg-muted",
        checked && "bg-muted/70 font-medium",
      )}>
      {children}
      <Check className={cn("size-3.5 shrink-0", checked ? "opacity-100" : "opacity-0")} aria-hidden />
    </button>
  );
}
