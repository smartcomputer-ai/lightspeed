import { StepDoneIcon } from "@/icons";
import { cn } from "@/lib/utils";

export function ProgressSteps<T extends string | number>({
  steps,
  current,
  onSelect,
  canSelect = (_step, index, currentIndex) => index <= currentIndex,
  label = "Progress",
}: {
  steps: ReadonlyArray<{ id: T; label: string }>;
  current: T;
  onSelect?: (step: T) => void;
  canSelect?: (step: { id: T; label: string }, index: number, currentIndex: number) => boolean;
  label?: string;
}) {
  const currentIndex = Math.max(0, steps.findIndex((step) => step.id === current));
  return (
    <div className="shrink-0 overflow-x-auto border-b px-5 py-3">
      <ol className="flex min-w-max items-center gap-5" aria-label={label}>
        {steps.map((step, index) => {
          const done = index < currentIndex;
          const active = step.id === current;
          const selectable = onSelect !== undefined && canSelect(step, index, currentIndex);
          return (
            <li key={step.id}>
              <button
                type="button"
                onClick={() => selectable && onSelect?.(step.id)}
                disabled={!selectable}
                aria-current={active ? "step" : undefined}
                className={cn(
                  "flex items-center gap-2 text-sm disabled:cursor-default",
                  active ? "font-semibold text-foreground" : "text-muted-foreground",
                  selectable && !active && "hover:text-foreground",
                )}
              >
                <span
                  className={cn(
                    "grid size-5 place-items-center rounded-full border text-[10px] font-mono",
                    done && "border-emerald-600 bg-emerald-600 text-white",
                    active && "border-primary text-primary",
                  )}
                >
                  {done ? <StepDoneIcon className="size-3" /> : index + 1}
                </span>
                {step.label}
              </button>
            </li>
          );
        })}
      </ol>
    </div>
  );
}
